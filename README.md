# Flux

A high-performance, safe, topology-aware filesystem transfer engine written in Rust.

Flux is inspired by FastCopy, but built on a different architecture — one designed
around correctness, bounded resident memory, persistent operation state,
deterministic traversal, resumability and filesystem-native acceleration.

**Status: early implementation.** `flux copy` (a file, or a folder's contents),
`flux verify` and `flux cleanup` work, with destination locking, resumable operations (`--resume`,
`--restart`) and cleanup of what an interrupted run left behind. `flux benchmark`
is not implemented yet. The authoritative design is
[`FLUX_FULL_UPDATED_SPEC_V16.md`](FLUX_FULL_UPDATED_SPEC_V16.md).

## Performance

![Flux against other copiers: the latest benchmark of main](https://ckir.github.io/flux/bench/latest.svg)

Each figure is Flux's median time divided by another copier's (robocopy on Windows; `cp` and `rsync` on Linux and
macOS), measured in the same GitHub Actions job: below 1.0, Flux is faster. Linux and macOS copy with a cold cache,
Windows with a warm one, and a pair appears only once a calibration shows its ratio is steady.
[The trend over time, and how it is measured.](https://ckir.github.io/flux/bench/)

## Goals

- High throughput, but **correctness before performance**
- Bounded resident RAM regardless of tree size
- Deterministic discovery and traversal ordering
- Hardlink topology preservation, including exclusion-aware dependencies
- Robust resume after interruption, with checkpoint validation: in a tree copy, a file of 64 MiB or more continues after the
  longest leading run of chunks whose digests match on both the source and the destination, and everything else is recopied; a single-file
  copy of 64 MiB or more continues a half-copied file from its proven prefix (the partial is re-read and hashed against the source); under `--durability=normal` a checkpoint is advisory and is validated against the
  data before it is trusted
- Source-mutation detection
- Atomic publication where the filesystem allows it
- Clear human and machine-readable (`--json`) reporting
- Cross-platform behaviour on Linux, macOS and Windows with explicit
  capability detection

## Pipeline

```
SOURCE
  → DETERMINISTIC SCANNER
  → SELECTION / FILTERING
  → BOUNDED DISCOVERY STREAM
  → TOPOLOGY + PLANNING STATE
  → BOUNDED TRANSFER QUEUE
  → PARALLEL WORKERS
  → STREAMING HASH / VERIFICATION
  → COMMIT / ATOMIC RENAME
  → REPORT
```

## Commands

```
flux copy
flux verify
flux benchmark
flux cleanup
```

### Hardlinks

`flux copy --hardlinks=auto|preserve|copy` (default `auto`) decides what happens to a source file that has several names
inside the copied tree. `auto` copies the first name met and links the other selected names to that copy, so the group
stays one object with several names; where a link is impossible the member is copied independently and reported.
`preserve` does the same but fails a member it cannot link (`HARDLINK_UNAVAILABLE`, `HARDLINK_IDENTITY_UNAVAILABLE`).
`copy` copies every name as a separate file. Only the names inside the copied tree are grouped, for fresh copies and for
`--resume`. The report counts links in `files_hardlinked` (not in `files_copied`) and adds `hardlinks_degraded` (members
copied independently) and `hardlinks_unchecked` (files whose link count the filesystem did not report), each absent at
zero; the summary line appends `; N hardlinked`, `; N hardlinks degraded` and `; N hardlinks unchecked` when non-zero.
`--dry-run` reports a planned link as `would_hardlink` and counts it in `files_hardlinked`. A hardlinked symlink is still
copied as independent links.

### Verification

`flux copy --verify[=none|source-stream|destination|full]` checks each copied file's content; `--verify` alone means
`destination`, and the default is `none`, which checks nothing. `source-stream` hashes the source bytes while they are
copied and compares nothing in this version. `destination` reads the written file again and compares it with the source
bytes that were read, so it detects a destination that returns different bytes than were written. `full` is `destination`
plus a second read of the source, which also detects source bytes that changed or were misread during the copy. The
re-read may be served by the operating system's page cache, so it does not prove the data reached stable storage
(`--durability` still governs syncing). A mismatch is reported as `VERIFY_MISMATCH` and counts as a failed file;
`--verify` cannot be combined with `--resume`. `--hash` selects the hash used by `--verify` (`blake3`, the default).

### Verify

`flux verify SOURCE DEST [--links=copy|skip] [--cross-filesystems] [--hash=blake3] [--json] [--verbose]` compares one
source (a file or a folder, mapped onto DEST as `flux copy` maps it) with DEST by content and writes nothing. Both
sides are walked together in path order and every file is read in full on both sides and hashed. Flux's own control
state under DEST (`.flux/`, the root lock, partial files) is never compared. Each path is reported as one of:

- `match`: the same on both sides (printed only with `--verbose`).
- `missing`: in the source, absent at the destination.
- `mismatched`: content, symlink payload or object type differs (`VERIFY_MISMATCH`).
- `extra`: at the destination, not in the source.
- `unreadable`: either side could not be read or hashed (the error is reported).

`--links=copy` (the default) compares a symlink's payload; `--links=skip` leaves source symlinks out. A directory that
is another mount is reported as present and not entered unless `--cross-filesystems` is given. Records go to stdout.

Exit status is 0 when nothing is missing, mismatched or unreadable, 1 when something is, and 2 for a usage error or
roots that are the same or nested. `extra` paths never change the exit status.

A destination-only directory that cannot be listed is reported as `extra` with the error and a stderr warning; it never
fails the run. A source path that is named like flux control state (`.flux/`, `.flux-root.lock`,
`*.flux-partial.<id>`) is reported `missing` because verify ignores that name at the destination. In a record, `path`
is shown with `\\`, control characters and non-UTF-8 bytes escaped, so two different names never print alike. If
stdout cannot be written to (a closed pipe), the run stops at once with exit 1.

`--json` writes newline-delimited JSON on stdout: one object per record (`path`, `outcome`, `kind`, `code`, `detail`),
then one final `{"summary": {...}}` object with the fields `source`, `dest`, `hash`, `matched`, `missing`,
`mismatched`, `extra`, `unreadable`, `not_entered`, `bytes_hashed` and `exit`. A run that stops before any walk with
exit 1 prints a summary of all-zero counts with `"exit": 1`, so a script must read `exit`, not recompute it from the
counts.

What a pass guarantees:

- `match` means the bytes the operating system returned for both files, at the moment each was read, hash equal. The destination read may be served from the page cache, so it proves the destination returns those bytes, not that they are on stable storage (the same boundary as `copy --verify=destination`).

- Hardlinks are not compared. Two names of one source file copied as two independent files still `match`. Use `copy --hardlinks` to preserve them.

- `extra` paths are reported but do not change the exit status. A script that needs an exact replica must read the summary (`extra` is a field of it).

- The result is a snapshot with no lock; a destination another run is changing can give stale results.

- A name that differs only in case on a case-insensitive destination is a `missing` plus an `extra`, not a `match`. The same holds for two Unicode normalisations of one name (macOS NFD against NFC): names are compared as raw bytes.

- A mount point the walker did not enter (no `--cross-filesystems`) is reported `match` for its presence only; its contents were NOT compared. The summary's `not_entered` count and the notice line say so, and exit 0 does not mean those contents were verified.

### Cleanup

`flux cleanup DEST [--dry-run] [--force] [--json]` lists the operations under `DEST/.flux/operations/` and the
destination's root lock. Statuses: `LIVE` (held by a running process, never touched), `RESUMABLE` (`--resume` can
continue it, kept; `--force` removes it), `STALE` (abandoned, debris, an orphan or moved-aside lock, or older than 7 days, removed), `COMPLETED_BUT_UNCLEAN` (finished with
leftovers, removed), `UNCERTAIN` (ownership or age unknown, reported only), `CORRUPT` (unreadable records, kept).
`--dry-run` deletes nothing; `--force` bypasses the 7-day retention only. Exit codes: 0 done, 1 a deletion or listing
failed or the lock was lost, 2 usage error, 3 refused as a whole. A normal `flux copy` finishes the cleanup of a previous run's recorded
leftovers by itself.

## Workspace

| Crate | Responsibility (spec §3) |
|---|---|
| `flux-core` | Scanner, selection, identity, topology, planner, scheduler, workers, copy, hardlinks, reflinks, sparse files, verification, resume, statistics, errors |
| `flux-fs` | Portable filesystem abstractions |
| `flux-platform` | Linux / macOS / Windows implementations |
| `flux-hash` | Streaming hashing (MVP: BLAKE3) |
| `flux-ui` | Progress and terminal presentation |
| `flux-cli` | Argument parsing and command dispatch — produces the `flux` binary |

## Building

Requires Rust 1.98.1+ (edition 2024).

```
cargo build --workspace
just check              # the local gate: fmt + clippy + typos + test
```

Tooling is listed in [`docs/dev-tooling.md`](docs/dev-tooling.md); see
[`CONTRIBUTING.md`](CONTRIBUTING.md) to get set up.

## Roadmap

See [`ROADMAP.md`](ROADMAP.md) for the 0.1 → 0.4 plan and
[`TODO.md`](TODO.md) for what is immediately next.

## Licence

[PolyForm Noncommercial License 1.0.0](LICENSE). Noncommercial use only.
