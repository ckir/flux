# Flux dev tooling

The toolchain audited against what Flux actually needs. Flux is an offline cross-platform CLI
file-copy engine.

Everything under "Standard tools" is **already installed on this machine** unless the
Status column says otherwise (verified 2026-09-09).

## Standard tools

| Tool | Config file | What it does for us | Status |
|---|---|---|---|
| `rustup` / `rustc` / `cargo` | `rust-toolchain.toml` | Pins the toolchain channel so CI and local agree. Pins `stable`. | 1.98.0 |
| `rustfmt` | `rustfmt.toml` | Formatting. `edition 2024`, `max_width 100`, `use_small_heuristics = "Max"`, `reorder_imports`. Gate is `cargo fmt --check`. | bundled |
| `clippy` | `clippy.toml` | Lints. Sets `msrv`, `allow-dbg-in-tests`, `allow-unwrap-in-tests`. Gate is `cargo clippy --workspace --all-targets -- -D warnings`. | bundled |
| `cargo-nextest` | — | Test runner used by CI (`cargo nextest run --workspace`). Per-test process isolation, which matters for Flux: tests touch real files, temp dirs, and permissions. Does **not** run doctests — pair with `cargo test --doc`. | 0.9.x |
| `cargo-deny` | `deny.toml` | Advisory/licence/ban/source auditing. Allow-lists MIT/Apache/BSD/ISC/Unicode/MPL/Zlib/CC0, denies `openssl-sys` in favour of rustls, denies unknown git sources. Targets list is per-platform — Flux must add macOS + aarch64 targets. | 0.16.x |
| `typos` (typos-cli) | `_typos.toml` | Prose + source spell-check, run as its own CI job. Needs a Flux-specific `extend-words` (e.g. `reflink`, `dedup`, `hardlink`, `ckir`). | 1.48.0 |
| `just` | `justfile` | Task runner; the single entry point for `build` / `test` / `clippy` / `fmt` / `check` / `clean` / `changelog` / `release`. The `default` recipe is `just check` = fmt-check + clippy + test. | 1.46.0 |
| `bacon` | `bacon.toml` | Background watcher during development (`check`, `check-all`, `clippy`, `clippy-all`, `test`, `doc` jobs; default `check-all`). | 3.25.0 |
| `lefthook` | `lefthook.yml` | Git hooks. Flux runs fmt + clippy on **pre-push only**, and invokes them as `just fmt-check` / `just clippy` / `just typos` so the hook and CI share one definition. Commits stay fast. | 2.1.12 |
| `git-cliff` | `cliff.toml` | Conventional-commit changelog generation (`just changelog`). Parsers for feat/fix/docs/perf/refactor/test/ci/chore. | 2.13.1 |
| `cargo-release` | `[workspace.metadata.release]` in root `Cargo.toml` | Lockstep version bump + `v{{version}}` tag across all workspace crates. | 0.25.x |
| `cargo-binstall` | — | How every one of the above gets installed without a source build. | 1.x |
| GitHub Actions | `.github/workflows/ci.yml` | Runs four jobs: `fmt`, `typos`, `clippy`, `test`. Uses `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`, `taiki-e/install-action@nextest`. | — |
| GitHub Actions release | `.github/workflows/release.yml` | Tag-triggered (`v*`) cross-platform binary matrix. Directly reusable for shipping the `flux` binary. | — |

## Other installed candidates for Flux

| Tool | Why it may matter for Flux | Status |
|---|---|---|
| `cargo-insta` | Snapshot tests. Good fit for the deterministic scanner ordering, the operation manifest, and CLI output. | installed |
| `cargo-machete` / `cargo-shear` / `cargo-udeps` | Unused-dependency detection across a 6-crate workspace. | installed |
| `cargo-mutants` | Mutation testing. Flux's `.gitignore` already ignores `mutants.out*`, so it was anticipated. Valuable for the invariant-heavy core (hardlink topology, resume, atomicity). | **NOT installed** — `cargo binstall -y cargo-mutants` |
| `cargo-zigbuild` | Cross-compiling to Linux/macOS targets from this Windows box. | installed |
| `criterion` (crate, not a binary) | Spec §3 asks for `benches/` and the CLI has a `flux benchmark` command. | add as dev-dep |

## Gate commands (the contract)

```
just check          # fmt-check + clippy + test — the local gate
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
cargo test --doc    # nextest does not run doctests
cargo deny check
typos
```
