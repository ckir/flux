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
- [x] **Symlinks are never recreated.** Closed by cut 10a (branch `spec/cut-10a-symlink-copying`): a symlink in a tree is recreated as a link with its payload verbatim (`--links=copy`, the default) or reported as skipped (`--links=skip`); `read_link`/`create_symlink` are on the filesystem traits; Windows creates links through `CreateSymbolicLinkW` with the unprivileged flag (Developer Mode; a plain user gets `SYMLINK_CREATION_UNAVAILABLE`, error 1314, measured), and an existing directory link is replaced atomically by a `FileRenameInformationEx` + POSIX-semantics retry. Left open, below.
- [ ] **A symlink given as SOURCE is still refused before the engine (K7).** `flux copy link DEST` exits 1 with `SYMLINK_CREATION_UNAVAILABLE`'s code and "a symlink SOURCE is not followed; copy its parent folder, or name its target"; copying a single link is a later cut (cut 10a, decision D1).
- [ ] **A symlink's own timestamps and permissions are not preserved**, even under `--preserve-times`/`--preserve-permissions` (cut 10a, decision D4; a deviation from §44.1's strictness, stated in `--help`): needs a no-follow times primitive (`utimensat(AT_SYMLINK_NOFOLLOW)` / a reparse-point handle) on every arm.
- [ ] **`symlinkat` on a Linux vfat/exFAT mount is unmeasured.** The arm maps `EPERM` to `SYMLINK_CREATION_UNAVAILABLE` and leaves `EACCES` as `PermissionDenied` (cut 10a, decision D8); the owner's vfat probe was not run (R-L1). Re-check before relying on it. Also unmeasured on Windows: a directory link replaced BY a directory link, a junction replaced by a file, and ReFS/FAT/SMB behaviour of the POSIX-semantics rename (probe cases are in `crates/flux-platform/src/symlink_probe.rs`).
- [ ] Hardlinked symlinks: every entry of a multiply-linked symlink inode becomes an independent symlink (the walker has no hardlink topology yet; §12.1 prescribes this for `--hardlinks=copy`).
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
- [ ] **§42 mount boundaries: the SOURCE half is done (cut 10b); the destination half remains.** SOURCE,
      DONE by cut 10b: the walk stops at a mount root, creates it at the destination as an empty
      directory, reports a `MountSkipped` record (JSON `mounts_skipped`), and `--cross-filesystems`
      restores the old behaviour (descend). **The destination half:** a bind mount INSIDE the destination can present a
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
the spec's numbers, 9-10 were learned during execution).

- [ ] **1. A copy that starts in the instant a cleanup (or a classification) probes the lock** is refused with
      `TARGET_LOCK_BUSY` (D1). Cleanup probes once per lock file it probes (the root
      lock and each `<lock>.broken.*`) and holds nothing across the listing.
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
      `skipped <id>: destination lock busy: <detail>` (the detail names "younger than the lease threshold") and exit 0.
      A younger orphan lock is refused as `ARTIFACT_OWNERSHIP_UNCERTAIN` and the pass exits 1.
- [ ] **10. A `<lock>.broken.*` file** is classified by its own record whether or not the workspace it names exists
      (spec row 11 wording corrected).

## Cut 9b debt

- [ ] `S251_1_close` stays `not-in-7a` only because `tests/model_impl_map.rs:141` needs one such label.
- [ ] The lock model has no notion of time or leases, so it does not capture the lease gate on the Dead and Orphan arms.
- [ ] The age test does not pin 86_399 -> 23h.
- [ ] A `<lock>.broken.*` row can show eligible while the root lock is busy (nothing is deleted: acquisition reports busy).
- [ ] `classify` now calls `identity()` before returning `Orphan`, so a failing identity call gives a copy an I/O error
      instead of its old exit-3 refusal.
- [ ] The lock model's `clean` process never creates a lock while the implementation does.
- [ ] A transient I/O error during the pass's re-read is reported as "changed since listing".
- [ ] `--json` has no fields for a failed listing or a lost lock (the cause is on stderr only).

## Cut 9c known limits

1. Under `--durability=normal` nothing is recoverable beyond 9a: a kill loses up to 1000 batched claims and their files are re-copied by the existing-file
   policy.
2. A format-2 store cannot be resumed by a binary older than this cut (`INCOMPATIBLE_STATE`; `--restart` or the newer binary).
3. A 9a or 9b operation resumed under 9c behaves as 9a (format 1, no notes).
4. Strict pays one extra synced commit per published file (two instead of one).
5. A note whose directory was renamed, replaced by a link or removed while the operation was down is UNCERTAIN or GONE by the matrix; the operator resolves it.
6. Single-file operations have no commit recovery yet (the rule is in the amendment); the J1 sweep and the existing-file policy still apply to them.
7. The walk is sequential today, so no other target can claim an entry between its `prepare` and its `commit_prepared`; a parallel walk must claim the
   entry at `prepare` time (insert-if-absent), which the row's key already allows.
8. `--restart` and `flux cleanup` never run commit recovery: they supersede or remove the operation, sweep its partials by id and never touch the
   object a note leaves in doubt.
9. A destination name that is not valid Unicode on Windows (an unpaired surrogate) cannot be recorded in a note; its publication under Strict reads as an
   invalid note at --resume and only --restart clears it.
10. The parent directory of an UNCERTAIN note may be on a case-insensitive filesystem under another spelling: the directory is opened by the recorded
   spelling, so a different case reads as absent and the verdict is GONE or UNCERTAIN, never RENAMED.

## Cut 9c debt

- [ ] A test for a heartbeat failure after the note is written is missing.
- [ ] The Normal path builds the note template needlessly.
- [ ] After a lost guard the UNBATCHED Strict path's `discard_note_on_failure` still writes `state.db` (`discard_prepared`,
      `crates/flux-core/src/tree.rs`), where the batched path writes nothing after a lost lock (9d spec decision 8). Align it:
      skip the discard when the guard reported the loss. (Triage of 2026-10-11, verified 2026-10-10.)

## Cut 9d known limits

1. A tree with one small file per directory gets no batching: the batch flushes at every `DirEnd`. Keeping directory handles alive across `DirEnd` is
   the later fix.
2. A crash loses up to `BATCH_FILES` files' staging work (they are recopied at `--resume`) and up to that many claims reach recovery as notes. Large files (64 MiB and up) in a tree now resume from validated checkpoints (cut 9f-1); small files in a batch are still recopied.
3. The rename is still not directory-synced (cut 9c limit 1 stands).
4. The data `fsync` per file is not batched; it is the floor of about 1 sync per file.
5. The age bound is not a timer: it is checked only at the NEXT STAGING in that directory or at its `DirEnd`. A long file being copied delays the
   publication of the files staged before it, unless it is at least `BATCH_BYTES` long. Because the walk is depth-first, a parent's pending files
   wait for the parent's next staging or `DirEnd`, i.e. until the whole subtree below has been walked, so their wait has no time bound.
6. A Replace target (an existing destination file, `Plan::Replace`) still pays one synced commit for its `Existing` claim in `before_create`, because
   `insert_if_absent` is Immediate under Strict (`crates/flux-platform/src/claims.rs`, `next_commit`): a replaced file costs 4 syncs today (data,
   `Existing` claim, note, claim) and about 2 after this cut (data, `Existing` claim). Folding the `Existing` claim into `prepare_many` is the later
   fix; it needs the exclusivity the claim gives to come from the in-frame pending names instead.
7. A file and a directory in one directory whose names fold equal only by a NON-ASCII fold, onto a folding destination: the file's rename meets the
   collision (batched) where an unbatched copy publishes the file and refuses the directory. Nothing is lost or overwritten; which of the two is
   reported differs.

## Cut 9f-1 open

- [ ] Single-file WAL layout: a single-file operation has no workspace directory, so a half-copied single large file still recopies on `--resume`; the next cut's FIRST decision is where its WAL lives (V16 s18.1/s119 give a single file no `wal/`; needs a V16 amendment).
- [ ] Verify the Strict duplicate-sync behaviour per platform (spec decision 6): macOS `F_FULLFSYNC` per descriptor, and Windows `FlushFileBuffers` needing write access on a duplicate; not performed in plan 2 (use the `verifying-platform-assumptions` skill).
- [ ] Salvage the valid prefix of a log with mid-log damage: today the whole segment is set aside and every large file recopies (owner ruling 23).
- [ ] An `--on-wal-corrupt=stop|quarantine` flag, for operators who want a damaged log to stop the run instead of being set aside.
- [ ] Identity gate and `st_dev`: resume identity compares `identity_text`, which embeds `st_dev` on Unix, and that may differ after a reboot on some filesystems, discarding every checkpoint; verify with the `verifying-platform-assumptions` skill.
- [ ] The WAL writer thread has no section-99 lock check of its own: it mutates the destination (`ensure_wal`, appends, Strict syncs) on its 1 s timer, and the run notices a lost lock only at its next guard or heartbeat (up to 5 s, or right after a process freeze), so a successor that already holds the lock can see a late flush from the old holder (design deviation (f)). Harm is bounded: replay reports `WAL_CORRUPT` and the file recopies, digests still protect destination bytes. Needs a `Send + Sync` guard handed to `WalConfig` (`guarded()` uses the main thread's lock handle).
- [ ] The Strict duplicate descriptor is not counted against `OPEN_WRITERS_MAX` (decision 6 followed without a recorded deviation until the plan 2 review).
- [ ] Reporting: a file whose checkpoints were discarded but which then FAILS gets no discard line (the `Err` carries no `Reuse`, `split_reuse`, `tree.rs`), and `--json` carries none of the reuse/discard fields; this weakens decision 12 ("nothing is discarded silently").
- [ ] `flux copy` with an unreadable `DEST/.flux` (permission denied) is refused whole by `check_control_plane` (`prior.rs:212`) instead of the spec's "cleanup cannot list: exit 1, classification incomplete" for `flux cleanup`; found by reading in cut 9b, not measured.
- [ ] CI: no job in `.github/workflows/ci.yml` sets `timeout-minutes` (the model and bench workflows do), so a test that blocks forever runs for GitHub's 6 h default; the nextest override covers `wal::` tests only. The Windows store tests legitimately run 112-158 s, so pick the limit from CI history.
- [x] Checkpoint hashing cost on many large files (both durabilities): `--durability strict` over 100 files of 64 MiB is about 15% slower than before cut 9f-1 plan 2 (median 22.54 s versus 19.58 s, `cur` = `ad5856b` code, `pre` = `97bd8d7`; 6 runs each, 3 per pass over 2 passes alternating binary order, page cache dropped per run; per-pass ratio 1.26 and 1.27; ranges 18.18-26.32 s and 14.72-20.51 s, so read it as roughly 10-25%). The same timed runs gave Normal 0.96x and Strict over the repository `mixed` case 0.95x, but the later profiles (below) show Normal pays the same hashing, so those timed Normal and `mixed` figures are noise-limited and do not show Normal is unaffected. `strace -f -c` counts 313 `fsync` calls versus 111 for the 100-file case (about 2 more per file; `mixed`: 517 versus 511) but only about 0.16 s more `fsync` time, so the extra syncs do not by themselves explain the gap; the cause of the wall gap is not confirmed. A `perf stat`/`perf record` pass (2026-10-15, same case, symbol builds of `97bd8d7` and `f52f5bd`, 2 rounds each, `perf` run as root) found user CPU 4.4-4.5 s versus 0.30 s, with `blake3_hash_many_avx2` the top symbol at 27.5% of samples on the `flux` main thread (the `flux-wal` thread 5.8%), so the extra CPU is the chunk digesting for checkpoints; system CPU was lower (12.8-14.0 s versus 15.0-16.2 s). Wall time UNDER `perf` was equal (23.0 and 25.0 s versus 23.3 and 26.1 s), so the +15% wall gap was not reproduced there; `perf` perturbs timing, so it is not shown that hashing alone is the Strict wall gap. A second pass profiled Normal the same way (2 rounds): user CPU 4.50 and 4.53 s versus 0.21 and 0.22 s, `blake3_hash_many_avx2` 28.7% of samples (`flux` 99.7%, `flux-wal` 0.27%), and wall under `perf` 21.6 and 22.4 s versus 19.4 and 19.8 s (about +11-13%). So the extra user CPU, about 4.3 s on this case, is checkpoint hashing on the copy thread in BOTH durabilities; the wall effect is not settled (Strict showed none under `perf`, Normal showed +11-13%, the unprofiled timed runs the reverse). Measured 2026-10-15 at the top level on an ext4 8-vCPU VM with the idle census at 0.03-0.06 and a busy-core control at 0.149; two `agy` sessions, `claude` and `sshd` ran uncontrolled. An earlier run the same day was contaminated by a `pnpm` build (Strict 1.11x, Normal 1.06x) and is not evidence. Closed by cut 9h (PR #101, merge `98925e2`): under Normal the hashing moved to a per-file worker thread (100 files of 64 MiB, wall median 31.80 s to 26.06 s, 4 runs x 2 passes, machine not idle; spec section 11). Strict still hashes inline on the copy thread by owner ruling R15 (no wall gain beyond the spread and about 22% more CPU with the worker), so the Strict cost described above remains; reopen if Strict wall time on many large files matters.

## Cut 9f-1 test debt (audits of plans 1 and 2; owner deferred)

- [ ] No test opens a FIFO through `DirHandle::open_existing`: the `O_NONBLOCK`/`ENXIO` classification (`dir_unix.rs:180`) is unpinned.
- [ ] No Windows test holds a hard-link keep-alive across `open_existing` to pin the identity check against a replaced object (`dir_windows.rs:637`).
- [ ] Torn-tail classification (ruling 23, `wal/replay.rs:84`) is pinned by a handful of cases; add multi-record spans, window-boundary damage and zero-tail variants.
- [ ] A property or fuzz test over `wal::replay::scan` / `load_file`: arbitrary prefix truncation and bit flips never panic, and `scan(stream[..valid_len])` is idempotent with no discarded tail (suggested by capstone r2 and r3 and the plan 1 audit).
- [ ] `crash_case_2_during_destination_flush` (`run/tests.rs`) zeroes a partial tail that `decide` never reads (its WAL holds only `FileBegin`, so `decide` returns Fresh before reading); it passes with or without the zeroing. Unverified by mutation; `assert_safe_redo` still pins the redo.
- [ ] No test injects a heartbeat failure during `plan_resume` (`copy.rs:597-608` maps it to `CopyStep::Heartbeat`; `decide.rs` covers it in isolation).
- [ ] FaultFs gaps that limit those tests: `FakeHandle::write` records nothing in the call log, so no test can order a WAL write against a sync or rename (`fault_fs.rs:688`); there is no read-fault injector, so a source read error mid-file cannot be simulated (the abort-on-source-read-error path is unexercised); `FakeDirHandle::open_existing` and `open_dir` push their call-log entry directly or not at all, so `on_nth` hooks and `fail(..)` never fire for them (`fault_fs.rs:1751`, `:1812`); `FakeHandle::identity` looks up the handle's stale path, so after `move_object` an open handle reads `Unavailable` where a real file reads its descriptor (`fault_fs.rs:749`).

## Cut 10a test debt (symlink copying; reviewer minors deferred by the SDD run, 2026-10-16)

- [ ] Fake (`fault_fs.rs`): the rename path has no test for a directory containing a link (`rekey(&mut g.symlinks, ..)`) or a file renamed over a link; `create_symlink_refuses_an_occupied_name…` cannot isolate the dangling-link case; `set_type(path, Symlink)` leaves no payload (`read_link` on it is `InvalidInput`); the fake folds ASCII case only, so a non-ASCII fold of the temp probe (`stage_into_batch`) is not fake-testable.
- [ ] `tree.rs`: the Strict REPLACE link note/discard (`publish_link`) has no test (both Strict note test parts publish New); the `record_claim` test should assert the `claim_upgrade(` count after the foreign case; the guard before `create_link`'s create is not pinned by the lost-lock test; the retry condition duplicates `stage_into_batch`'s; `link_one` repeats ~200 lines of `copy_one`'s resolve/claim/note/record code (drift risk).
- [ ] `run/resume.rs`: `each_exact_match_option_is_compared_and_named` has no `links` row (a one-directional comparison survives; needs a skip-built prior); no CLI-level test of `--resume --links=<other>`'s exit code; the CLI `--links=skip` test asserts the skip line only for `rel` and not JSON `files_skipped`; the second-run `--update` test checks counts only.
- [ ] `run/tests.rs`: the crash-resume link test's guard-before-removal check accepts any earlier `metadata(LOCK)` (use `rposition` after the gate's source metadata); nothing pins that the partial sweep still skips `WalkEvent::Other` (a FIFO at a partial name); `CLEANUP_NOW`/`cleanup_cfg` duplicate `cleanup::delete`'s test helpers.
- [ ] `crates/flux-platform/tests/dir_handle.rs`: the `rename_replace` link test asserts only `is_symlink()` after the replace; the empty-payload kind is asserted on Linux only; the Windows tests skip on the hosted runner (it cannot create links), so the success path rides on the owner's Developer-Mode box (see `docs/accepted-boundaries.md`).
- [ ] Windows arm: `map_symlink_error` is Windows-only (not Linux-unit-testable); the identity re-check window in `create_symlink` could be narrowed by holding the opened path without `FILE_SHARE_DELETE`; a failing `FileRenameInformationEx` retry reports the retry's status, not the first `ACCESS_DENIED` (an unsupported-class status on FAT/old SMB would read as "invalid info class"); WSL `LX_SYMLINK` sources are refused as special files with no hint why; the K7 refusal reuses `SYMLINK_CREATION_UNAVAILABLE`'s code though this version can create links (spec 3.6; a later cut may add a dedicated code); a Windows `flux copy` smoke test by the owner is still owed.
- [ ] Anomalies verified during the run and left for triage in the local inbox: `discard_note_on_failure` discards the prepared note on any publish error, a lost lock included (pre-existing for files); the fake folds ASCII case only.

## Cut 10b known limits (source mount boundaries; spec `docs/superpowers/specs/2026-10-17-cut-10b-mount-boundaries-design.md`, sections 3.2, 4, 6)

- [ ] **On a Linux kernel before 5.8 (or any place `statx` is unavailable) every directory on the SAME device as its parent answers `Unknown`** (a directory on a different device still answers `Yes`). Default safety copies as before with one grouped warning; Strict refuses the whole operation at the first such subdirectory. Use Default, or a newer kernel. On the SOURCE side `statx` answering EPERM (an old seccomp profile) now also falls back to the device comparison.
- [ ] **The DESTINATION arm `DirHandle::mount_root` (`crates/flux-platform/src/dir_unix.rs`, ~:535) still treats `statx` EPERM as an error.** Pre-existing; not changed by this cut.
- [ ] **A hung network mount under the source still blocks `flux copy` at the stat of the mount point** (the query and `metadata` both ask the mounted filesystem; nothing below it is read).
- [ ] **On macOS `/Users` and `/Applications` sit on the Data volume**, a different device from the sealed system volume, so `flux copy / DEST` skips them as mounts by default (as `find -x /` does).
- [ ] **`--cross-filesystems` on a single-FILE copy is accepted and recorded in its state although it has no effect;** a resume of that file with the other setting is INCOMPATIBLE_STATE (as `--links` behaves).
- [ ] **A state written by a PRE-cut binary (no key, reads as false) resumed by this version runs under the `Stay` default although the old binary always crossed.** `--resume` of an operation that had half-copied a mount skips that mount with a `MountSkipped` line and leaves the half-copied directory (exit 0). `--resume --cross-filesystems` is refused as INCOMPATIBLE_STATE, so finishing in kind needs `--restart --cross-filesystems`.
- [ ] **autofs trigger directories are UNMEASURED.** The query passes `AT_NO_AUTOMOUNT`; whether an unmounted trigger reads as a mount root, or `read_dir` mounts it on descent, is not known.
- [ ] **A Windows mounted-volume folder under a copied tree is UNMEASURED** (the source walk already types it as a link or special file).
- [ ] **`STATX_ATTR_MOUNT_ROOT` on a real same-filesystem bind mount is unmeasured** (needs privilege; the platform tests use the real mount roots `/proc` and `/dev/shm`).
- [ ] **A skipped mount's empty directory has the process-default attributes**, because no tree copy applies directory metadata yet (cut 4b design decision 8). When directory metadata is built, the source for a mount is the mounted root's (as rsync documents).
- [ ] **No `-xx`-style mode** that omits mount points entirely.
- [x] **The cost of the one extra `statx` per directory: measured 2026-10-17 at the top level, owner-approved, CPU about 87% idle with background Xorg, opencode and Claude uncontrolled. A local, uncommitted variant of `small_1000x4kib` with one directory per file (1000 directories) ran twice per arm (mount check on vs `--cross-filesystems`). normal: 2.05 s and 2.16 s on vs 1.87 s and 2.03 s off; strict: 8.36 s and 8.61 s on vs 9.15 s and 8.83 s off. Each run's own interval is about +-10%, criterion p = 0.07 to 0.68, and the sign is not consistent, so no cost is detectable; the reasoned bound is 1000 `statx` calls (a few microseconds each, not measured directly) against about 2 s, at most 0.2%. The committed bench has only 20 directories and cannot resolve it.

## Cut 10b test debt (source mount boundaries; reviewer minors deferred by the SDD run, 2026-10-17)

- [ ] `walk.rs` tests: `paths`/`paths_with` duplicate the event renderer.
- [ ] `FaultFs::mount_root` ignores its `parent` argument, so no test pins what the walk passes.
- [ ] A mutant that moves the mount query before the `file_type != Dir` refusal is caught by no test.
- [ ] The Default branch of `MountUnknown` is not covered under a non-live frame.
- [ ] `source_mount_unknown` aggregation beyond count 1 is covered only by the `DegradedGroup` unit tests.
- [ ] The engine test for `Unknown` packs three scenarios into one test.
- [ ] `is_zero` is a generic free-function name in `report.rs`.
- [ ] The device-number FALLBACK of `StdFileSystem::mount_root` is exercised by no `std_fs` test on Linux (only the helper's unit test), and two platform tests assume a kernel of 5.8 or later.
- [ ] The doc line on the unix `mount_root` in `crates/flux-platform/src/std_fs.rs` (~:260) is 120 columns wide.
- [ ] No real-process CLI test creates a mount (needs privilege), so the end-to-end skip is pinned only through the fake.

## Main review of 2026-10-17 (agy, new model; brief `.clavity/seams/main-review-r1.md`; every finding re-measured by the driver)

PS-1 (a socket at the lock or claim-store path was `IO_ERROR`, exit 1) and PP-1 (`ClaimStore::get` answered `Ok(None)` for an undecodable record) were fixed in `31a000d`. Left open:

- [ ] **A lost lock inside the STOP-HELD discard loop of `flush_batch`** (`crates/flux-core/src/tree.rs`, `let _ = claims.borrow_mut().apply_recovery(&ops);`): when the first stop is not `TargetLockBusy` and `cx.guard` then fails while a LATER entry's temporary is discarded, the loop still pushes that entry's `Discard` and writes `state.db`, and returns the original stop, so `drain_batches` removes instead of keeping. A narrow race, the same family as the UNBATCHED item above (9d decision 8 says the batched path writes nothing after a lost lock). Reasoned from the code, not reproduced.
- [ ] **The non-ASCII fold limit (cut 9d limit 7) also covers two FILES** whose names fold equal only by a non-ASCII fold, under `--existing=update` or `skip-existing` into a folding destination: the second resolves against the not-yet-replaced destination entry (the pre-check `folded()` is ASCII only and the temp probe runs later) and may be skipped instead of reported as a collision. Reasoned; the fake folds ASCII only and this machine's filesystem is case-sensitive.
- [ ] **The fake (`fault_fs.rs`) does not match the two fixes of `31a000d`:** `corrupt_claim()` (used by `FakeClaimStore::get`) answers `Code::IoError` where `RedbClaimStore::read` now answers `StateCorrupt`, and `FakeDirHandle::open_claim_store` has no special-file check (unlike `FakeDirHandle::open_lock`), so `add_special` at `state.db` answers `StateCorrupt` ("zero-length claim store") instead of `DestinationError`. No fake-backed test reaches either today. Found by the capstone, below its floor. `RedbClaimStore::insert_if_absent` and `upgrade_own_claim` still map an undecodable record to `IoError` (pre-existing).
- [ ] **Windows `open_lock` / `open_claim_store` with an `AF_UNIX` socket file at the name are unmeasured** (`dir_windows.rs`, `open_file_at`; the new socket tests are POSIX-only). The Windows open refuses reparse points, which an `AF_UNIX` socket file is, but the code returned by that path was not checked.

## Cut 11a known limits (`flux copy --dry-run`; spec `docs/superpowers/specs/2026-10-17-cut-11a-dry-run-design.md`, sections 1, 3.5, 3.6, 4)

- [ ] **Prior operations at DEST are noted, not previewed.** A resumable prior, a completed prior and an unreadable record each print one `note:` line and the fresh plan still prints; the exit code never changes for it, and the JSON carries `prior_operation` in `unprobed`. What a real run does with the prior (it may refuse with `RESUMABLE_OPERATION_EXISTS`) is cut 11b's `--dry-run --resume|--restart|--break-lock`, which this cut refuses as usage (exit 2).
- [ ] **Same-run namespace collisions are not predicted (spec 5.2).** Two source names that fold onto one destination entry plan as two creates where a real run reports one collision.
- [ ] **Failures that occur only while bytes move are not predicted:** a read error mid-file, a full disk, a source changed during the copy, metadata-apply complaints, claim-store write failures. An unreadable source FILE (open refused, e.g. mode 000) is predicted as `would_copy`: I3 forbids opening the source, and permission bits are not a portable predictor (final review I-3b).
- [x] **A top-level source entry named `.flux` now mirrors the live run (ruling P11, retiring P6):** a directory merges (no `would_create` line, no count; its reserved children `operations`, `standalone`, `atomic` refuse as in a real run and its other children plan), and a file or link fails as live does (`DESTINATION_ERROR` into an existing DEST, `SAFETY_REJECTED` into an absent one, step `gate`). A `.flux` deeper than the root's direct children is judged against the destination as any other name.
- [ ] **One stderr line per entry and no quiet flag (D8).** The unprobed `NOREPLACE_PUBLISH_UNAVAILABLE` note prints on every tree dry run that reaches the walk (D3), never for a single file. Both kinds of run also report `target_lock` in `unprobed` with one stderr line: no lock was taken and no state written, so a real run may be refused or fail there (ruling P10, master spec 5.2).
- [ ] **A temporary name over the limit is predicted (ruling P12):** a source name whose temporary (`temp_path`) exceeds `NAME_LIMIT` fails as `IO_ERROR` at step `create`, as live does at `create_new`; other name-length failures are not predicted.
- [ ] **A tree source onto a DEST that is a regular file** is the CLI resolver's usage error (exit 2, no JSON) in both modes; a DEST below a regular file is the engine's pre-flight refusal (exit 1, and under `--json` the dry run's object carries `dry_run: true`) (ruling P8).
- [ ] **The dry mode's cost on the live path was not measured (D9):** one `dry_run` branch per entry, and the live path now also calls its claim store through `dyn ClaimStore` (a vtable call per claim operation).

## Cut 11a test debt (deferred by the SDD run, 2026-10-17)

- [ ] **The fake does not inherit identity strength.** `crates/flux-core/src/fault_fs.rs` gives every directory it creates a Strong identity regardless of its parent's, so a fake-backed LIVE run into a weak destination notes `replace_degraded` only for the root, while the dry run notes the planned child too (the spec's inheritance rule; the dry test asserts count 2). On a real filesystem both note 2. Debt: let the fake inherit identity strength on `create_dir` (ruling P4).
- [ ] **Untested arms and pins:** `dry_file`'s `Err` arm is untested (the directory-target refusal happens in `prepare_file` before the dry branch); the unreadable-note test pins only `": "`; two of the three note strings are pinned by inspection only (the completed-prior one is pinned end to end by the CLI test).
- [ ] **Swap-blind `plan_file` tests.** The `plan_file` tests would not catch a swap of the source check and the identity gate (a fixture with a special source AND a directory at the name would); `gate_link_read`'s `None` branch is exercised only through the tree dry stops.
- [ ] **The real-filesystem paired corpus** (`crates/flux-core/tests/dry_run_std_fs.rs`) has no case that ends `Err` (DEST inside SOURCE would); its changed-set filter drops only Flux's own control entries (`is_control`); the snapshot is kind, len, mtime and link payload, with no byte comparison of file content, although the test name says byte-for-byte; the held-lock test has no negative control; the memory bound (`peak_b < 2 * peak_a`, measured 7693/11637 bytes) has about 24% margin and counts every thread.
- [ ] **CLI coverage:** the dry/live summary selection is repeated in the tree and file branches of `crates/flux-cli/src/main.rs`.
- [ ] **Leftovers from the run:** `NullClaimStore` derives `Clone, Copy, Default` beyond the plan; `copy.rs`'s `FilePlan` and `tree.rs`'s `RootIdentity::Planned` carried temporary `expect(dead_code)` attributes between tasks (both removed).
- [ ] **A README section for `flux copy`'s flags** (there is none today; `--dry-run` is documented in `--help` only).

## Cut 9g open (single-file resume; triaged from the anomalies inbox 2026-10-14)

- [ ] `plan_resume` hashes through `parent.open_read(temp)` without comparing that handle's identity to `partial_meta.identity` (only `open_existing` checks it), so a concurrent double swap of the partial name could keep an unproven prefix. Pre-existing (same code as 9f-2 `full`), `copy.rs` near line 610; needs a second writer on the destination directory.
- [ ] Tooling (clavity agy driver, not flux code): the `[13b] TRUNCATED REPLY` flag fired falsely on complete `adversarial-panel-review` replies (twice), and `[13b] ECHO MISSING` fired when agy stripped the backticks from the quoted last line (once per capstone). Workaround: read the reply file tail, one permitted re-ask. Report upstream if it recurs.

## Cut 9d debt

- [ ] Keep directory handles alive across `DirEnd` so a tree with one small file per directory batches (limit 1 fix).
- [ ] Fold the Replace target's `Existing` claim into `prepare_many` (limit 6 fix); the exclusivity the claim gives must come from the in-frame pending
  names.
- [ ] The source recheck now happens at staging, not immediately before the rename: a source changed after the recheck is published as the snapshot
  read, up to 64 files wide (an unbatched copy already allows the gap between recheck and rename). The 1 s age is no bound on that gap: it is checked
  only at the next staging in that directory or its `DirEnd`, and a parent's pending files wait until the whole subtree below has been walked
  (limit 5), so their gap has no time bound.
- [ ] Strict small files are 34-35% faster, not the 40% the spec gated on (measured 2026-10-10: 3.0 -> 1.04 syncs per file, so batches are
  near full and the age trigger is not the limit). The remaining time is the one data `fsync` per file (about 70% of syscall time under
  `perf trace`, about 6 ms each): batching removed the cheaper claim `fdatasync` calls. Going further needs a batched or overlapped data sync
  (`syncfs` per batch, parallel fsync), which changes what "durable before publish" means: its own design and cut.
- [ ] Hardening: a lost lock noticed only inside a cleanup `discard` (heartbeat-stop cleanup, per-entry `prepare` fallback, and cut 9e's failed-sync discard in `flush_batch`) does not stop the flush,
  so store writes can still happen before the next guard (recovery handles every resulting state; spec decision 8's "writes nothing" holds for the
  guard-detected loss). (The `prepare_many` fallback's stop, a store error carrying `TargetLockBusy`, no longer drops the remaining entries: each is
  reported with its temporary kept, `a_stop_in_the_per_entry_prepare_fallback_reports_every_other_entry`.)
- [ ] Tests not written: a lost lock inside `copy_one` with pending entries in two frames (the `DirEnd` form, a lost lock at a child's `DirEnd`
  flush, is covered by `an_abort_drains_every_frame`; the `copy_one` form is not); the `discard_prepared` fallback after a failed rename when
  `apply_recovery` also fails; the É/é non-ASCII fold on a real folding filesystem (the fake folds ASCII only; reasoned, not measured).

## Cut 9e known limits

1. First threads in the engine. The shipped binary builds with `panic = "abort"` (`Cargo.toml:111`), so a worker panic is a hard crash, not a
   reported failure; it is safe for the recovery matrix (no note exists yet, the sweep removes the temps) but is not a handled error. In
   unwinding test builds the panic propagates out of the scope and fails the test. No catch-and-report path is built.
2. The barrier lengthens the window between the temps' creation and the notes by up to the slowest sync; the post-barrier guard bounds the lock
   risk, not the source-change window (9d known limit 5 still applies).
3. A shallow pending batch still waits while a subtree is walked (9d limit 5), now holding descriptors; the cap in decision 7 bounds that.
4. The gain is measured on Linux ext4 only.
5. One file per directory gets no benefit (9d limit 1).
6. The heartbeat is itself a synced write (`lock/held.rs:48`), so on a filesystem where a burst of concurrent `fsync` calls holds the journal, the
   heartbeat can queue behind the barrier and arrive late. Today's per-file `fsync` has the same exposure at a smaller scale. Not measured here:
   the acceptance run records the longest barrier and the longest heartbeat gap, and reports them.
7. A `sync_all` that hangs (a dead network mount) hangs the run exactly as today's inline sync does: the system call is not cancellable. The
   main thread keeps the heartbeat going during the wait, so the lease stays alive; no timeout or cancel is built, and none is claimed.

## Cut 9e debt

- [ ] 9e-2: a Linux `syncfs(2)` on the destination filesystem at the barrier (measured 93-94% faster in the spike) ships only if (a) a capability
  probe not depending on the kernel version string shows the kernel reports writeback errors from `syncfs` (5.8 and later), and (b) `syncfs` is
  measured under a heavy (tens of GiB) background writer; otherwise it is dropped and recorded here. See the 9e spec, decision 9.
- [x] The drain after a lost-lock or heartbeat-failure stop ran the data-sync barrier (and, after a heartbeat failure, published): fixed by owner ruling B2' (9e spec decision 10).
- [ ] The acceptance measurement's figures (the longest barrier, the longest heartbeat gap, cap flushes; Strict small/flat ratios against the 9d head)
  are filled in by Task 7 of the 9e plan.
- [x] (closed 2026-10-12: hooks are keyed by (name, nth), pinned by `two_hooks_on_one_name_at_different_counts_both_run`) `FaultFs::on_nth` keeps one hook per call name (`hooks.insert` replaces, `crates/flux-core/src/fault_fs.rs`); a second `on_nth` on the same name
  silently wins. Document or key by (name, nth).

- [ ] A cross-platform real-filesystem Strict test copying a READ-ONLY source with an mtime check: the inline sync now runs after
  `set_permissions`/`set_times`, and no CI test exercises `FlushFileBuffers`/`F_FULLFSYNC` after a handle-level read-only attribute.
- [x] Error precedence in the non-batched Strict path: when both the `Preserve::Strict` metadata step and the sync fail, `MetadataApplyFailed`
  is reported instead of `StrictDurabilityUnavailable` (a consequence of the sync moving after the metadata step). Owner ruling 2026-10-12: kept; the
  first failing step reports its own code, in step order, and the sync never runs after a Strict metadata failure. Pinned by
  `a_strict_metadata_failure_is_reported_before_the_sync_runs`.
- [ ] The end-to-end stall oracles pin `FLUX_TEST_BATCH_AGE_MS` and `FLUX_TEST_OPEN_WRITERS` so no age or cap flush moves a guard index; a clock injectable from the CLI (the tree already takes `BatchPolicy.now`) would let an end-to-end test exercise an age flush deterministically instead of blinding the oracles to it (capstone round 3 recommendation).
- [ ] Test-audit debt (cut 9e, 2026-10-11): the suite pins the drain's stop classes with hand-built two- and three-frame fixtures; a property test over (frame depth x stop class x failure phase) asserting "no temporary neither removed nor reported", "no note for unsynced data" and "no barrier after a stop", and a real-filesystem crash-injection harness (the fake masks OS fsync semantics), would cover the state space the point tests cannot.
- [ ] `Run.beat_gap_max` is refreshed after the copy only in the tree run (`crates/flux-core/src/run/mod.rs`); the single-file run
      keeps its lock-time value. Refresh it after the single-file copy, or document the field as tree-only. (Triage of 2026-10-11.)
- [ ] The walk holds one open `DirHandle` per live frame (`Frame::Live { dir, .. }` in `crates/flux-core/src/tree.rs`, since e2112e3),
      so a tree deeper than the soft descriptor limit minus the run's own handles (about 1000 levels at the 1024 default) fails with
      "too many open files"; the 9e open-writer cap counts writers only. Count live frames in the budget, or refuse with a clear
      error near the limit. (Found by the 9e capstone, round 2; triage of 2026-10-11.)

## Scaffolding follow-ups

- [ ] Run `lefthook install` in each clone (or add it to a bootstrap recipe)
- [x] Replace the placeholder `benches/copy.rs` once there is a pipeline to measure (#76: in-process criterion,
      Normal and Strict, real-disk data dir, tmpfs refused)
- [x] Replace the placeholder test in `tests/integration/mod.rs` with the first
      real case from spec §68.2 (#75: engine-level cases; CLI-surface cases stay in `crates/flux-cli/tests`)

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

Triage of 2026-10-11 adds two more.

- [ ] **`model-extended.yml` and `model-mutants.yml` download `tla2tools.jar` on every job.** Both run `models/lockproto/run.py`,
      but only `model.yml` got the jar cache step. Add the same cache step to both.
- [ ] **Watch item: `Test (windows-latest)` failed once on PR #80** in `crates/flux-cli/tests/recovery.rs` (two stall tests panicked at
      `recovery.rs:68` after ~61 s; the re-run of the job passed). Cause not found. If it recurs, treat it as a stall-wait timeout flake
      on Windows and raise the wait or make the stall-file check robust.

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
      4186 and 5005 of `FLUX_FULL_UPDATED_SPEC_V16.md`); `flux cleanup` carries it as `LEASE_THRESHOLD`
      (`crates/flux-core/src/cleanup/mod.rs`), and the run's own heartbeat constant is the 5 s `HEARTBEAT_INTERVAL`
      (`crates/flux-core/src/run/mod.rs`). Because the beat can lag the last call by up to 5 s,
      a chunk must complete within 25 s to keep the heartbeat under 30 s old: the worst case holds above about
      10 KiB/s of sustained throughput (256 KiB / 25 s = 10.24 KiB/s; it was about 2.6 KiB/s at 64 KiB). Very slow
      media could appear stale during one slow chunk, though staleness also requires that no live process holds the
      lock (section 102). This is a documented trade-off, not a defect.

## Deferred from cut 7b

Each is deferred to the cut that first reads it (cut 7b spec, `docs/superpowers/specs/2026-10-02-cut-7b-state-v2-and-heartbeat-design.md`, Scope).

- [ ] **`configuration_fingerprint` (§19, §121)** - cut 9: read only by resume; most §121 options do not exist yet.
- [ ] **Checkpoints and `last_checkpoint` (§19, §139, §148, §164)** - cuts 8/9: the spec defines them only through
      the WAL and `state.db` (§119's layout).
      Partly delivered by cut 9f-1: batched chunk checkpoints in a WAL, and partial-file resume for tree files of
      64 MiB and up. `--resume-verify=chunks|full` shipped in cut 9f-2. Single-file resume shipped in cut 9g: a single-file
      `--resume` that adopts a prior operation and whose source is at least 64 MiB re-reads and hashes the partial
      against the source (`chunks` runs as `full` and is reported as such) and continues from the proven prefix; it keeps
      no log and creates no file beside the target, and a partial survives a hard crash and any failure that stops the run before it writes anything (a lost
      heartbeat or lock, a read error while validating), and a failure after the copy has started removes it. Cut 9f-3 as first parked was NOT built, by ruling of 2026-10-13 (agy consulted, ALIGNED after one
      negotiation round; the owner approved): **closed, reopen if measured** - WAL compaction, rotation and snapshots,
      because the WAL grows by 32 KiB of digests plus about 5 KiB of framing per GiB at the default 16 chunks per
      `Range` (60 KiB at 4), under 0.006% of the payload, and the workspace is deleted on completion; reopen if a
      measured case exhausts storage. **Closed, reopen if the spec changes** - `--resume-verify=metadata` (the CLI keeps
      refusing it with exit 2) and the section 121 upgrade rule (test item 86, which fires only after digests were
      compacted away): reopen if the owner amends the spec to define what `metadata` verifies and its safety under
      `--durability=normal`. **Waived** - the WAL-bytes-per-GiB acceptance measurement promised by the 9f-1 design
      (decision 11), by the arithmetic above. Advisory, not acted on: agy recommends the owner retire `metadata` and
      section 121 from the spec, because trusting unhashed partial bytes after a crash under `normal` would publish
      unproven data; that argument was not measured, and the spec amendment is the owner's call. Decision 15's real-filesystem crash tests are the
      `real-filesystem crash-injection harness` item, still open, under "Cut 9e debt" above (the test-audit debt entry
      near line 593).
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
- `CLOSED-BY-RULING` **Kill after publish, before the record is finalised (single file), 2026-10-14.** A SIGKILL
  between the publishing rename and the COMPLETED record write leaves the target complete, no partial and the
  record Transferring; `--resume` then re-copies the whole file (measured: 4 GiB, `files_overwritten: 1`, no level
  line, 11.7 s, twice a fresh copy), replacing an identical file atomically, so the result is correct. Owner ruling
  (agy consulted first, ALIGNED): accept. The window is milliseconds, and the same on-disk state also arises from a
  crash before the temporary exists over a pre-existing target, so "Transferring, no partial, target present" cannot
  be read as "published". Rejected: an intent record before the rename (adds a synchronous record write to every
  single-file copy, and under `--durability normal` an identity match does not prove the bytes survived a power
  loss), hashing both sides on resume (reads everything for a state that is often an old, different target), and a
  size-and-mtime match (trusts unproven bytes). **Reopen if:** a measured case shows the window is hit routinely, or
  the single file gains a record write before the rename for another reason.
- `CLOSED-BY-RULING` **No SIGINT / SIGTERM / console-control handler (spec section 56), 2026-10-14.** No crate installs
  a handler, so a Ctrl-C kills the process like a crash: the lock is released by the OS (the kill-then-`--resume` e2e
  tests resume with no `--break-lock`), the staged partial is kept and the record stays TRANSFERRING; the next
  `--resume` or `--restart` recovers it. Owner ruling (agy consulted first, ALIGNED): accept, and amend the spec
  (new section 56.1; a note under section 132). Rejected: the spec's cancellation to `PAUSED` (a new `OpState`, state
  format version 5, resume/restart/cleanup changes, for a state `FAILED` already equals in behaviour), and a
  cooperative stop that keeps the partial (a normal stop through the error path DELETES the partial, `copy.rs`
  `StreamFail::Discard`; cross-thread cancellation plumbing for the guard, the WAL writer and the sync barrier; a stop
  can hang on a stalled read or an fsync). **Reopen if:** an engine caller (a library or UI) needs to cancel a run, or
  a measured case shows operators losing work to an interrupt.
