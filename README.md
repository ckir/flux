# Flux

A high-performance, safe, topology-aware filesystem transfer engine written in Rust.

Flux is inspired by FastCopy, but built on a different architecture — one designed
around correctness, bounded resident memory, persistent operation state,
deterministic traversal, resumability and filesystem-native acceleration.

**Status: pre-implementation.** The workspace and toolchain are scaffolded; no
engine code exists yet. The authoritative design is
[`FLUX_FULL_UPDATED_SPEC_V16.md`](FLUX_FULL_UPDATED_SPEC_V16.md).

## Goals

- High throughput, but **correctness before performance**
- Bounded resident RAM regardless of tree size
- Deterministic discovery and traversal ordering
- Hardlink topology preservation, including exclusion-aware dependencies
- Robust resume after interruption, with checkpoint validation
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

Requires Rust 1.85+ (edition 2024).

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
