# Flux dev tooling

The toolchain inherited from `aishelter` (`C:\Users\user\Development\Rust\aishelter`),
audited against what Flux actually needs. Flux is an offline cross-platform CLI
file-copy engine, so aishelter's server/DB/serverless tooling is dropped (see
"Deliberately not carried over" at the bottom).

Everything under "Carried over" is **already installed on this machine** unless the
Status column says otherwise (verified 2026-09-09).

## Carried over from aishelter

| Tool | Config file | What it does for us | Status |
|---|---|---|---|
| `rustup` / `rustc` / `cargo` | `rust-toolchain.toml` | Pins the toolchain channel so CI and local agree. aishelter pins `stable`. | 1.98.0 |
| `rustfmt` | `rustfmt.toml` | Formatting. aishelter: `edition 2024`, `max_width 100`, `use_small_heuristics = "Max"`, `reorder_imports`. Gate is `cargo fmt --check`. | bundled |
| `clippy` | `clippy.toml` | Lints. aishelter sets `msrv`, `allow-dbg-in-tests`, `allow-unwrap-in-tests`. Gate is `cargo clippy --workspace --all-targets -- -D warnings`. | bundled |
| `cargo-nextest` | — | Test runner used by CI (`cargo nextest run --workspace`). Per-test process isolation, which matters for Flux: tests touch real files, temp dirs, and permissions. Does **not** run doctests — pair with `cargo test --doc`. | 0.9.x |
| `cargo-deny` | `deny.toml` | Advisory/licence/ban/source auditing. aishelter allow-lists MIT/Apache/BSD/ISC/Unicode/MPL/Zlib/CC0, denies `openssl-sys` in favour of rustls, denies unknown git sources. Targets list is per-platform — Flux must add macOS + aarch64 targets. | 0.16.x |
| `typos` (typos-cli) | `_typos.toml` | Prose + source spell-check, run as its own CI job. Needs a Flux-specific `extend-words` (e.g. `reflink`, `dedup`, `hardlink`, `ckir`). | 1.48.0 |
| `just` | `justfile` | Task runner; the single entry point for `build` / `test` / `clippy` / `fmt` / `check` / `clean`. aishelter's `default` recipe is `just check` = fmt-check + clippy + test. | 1.46.0 |
| `bacon` | `bacon.toml` | Background watcher during development (`check`, `check-all`, `clippy`, `clippy-all`, `test`, `doc` jobs; default `check-all`). | 3.25.0 |
| `lefthook` | `lefthook.yml` | Git hooks. aishelter ran fmt + clippy on **both** pre-commit and pre-push. Flux runs them on **pre-push only**, and invokes them as `just fmt-check` / `just clippy` / `just typos` so the hook and CI share one definition. Commits stay fast. | 2.1.12 |
| `release-plz` | `release-plz.toml`, `.github/workflows/release-plz.yml` | Release automation, replacing aishelter's `git-cliff` + `cargo-release`. In CI it keeps one release PR open (shared version bump, `Cargo.lock`, `CHANGELOG.md` from commit messages, grouped by the `[changelog]` parsers); merging it creates the `v{version}` tag and GitHub release, and the tag starts `release.yml`. Git-only mode: versions come from tags, nothing goes to crates.io. Uses `PR_UPDATER_TOKEN` so CI runs on the PR and the tag triggers workflows. Local `release-plz update` previews the result. | 0.3.167 |
| `cargo-binstall` | — | How every one of the above gets installed without a source build. | 1.x |
| GitHub Actions | `.github/workflows/ci.yml` | aishelter runs four jobs: `fmt`, `typos`, `clippy`, `test`. Uses `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`, `taiki-e/install-action@nextest`. | — |
| GitHub Actions release | `.github/workflows/release.yml` | Tag-triggered (`v*`) cross-platform binary matrix. Flux's tags come from release-plz; the builds attach to the release it creates. | — |

## Installed here, not used by aishelter — candidates for Flux

| Tool | Why it may matter for Flux | Status |
|---|---|---|
| `cargo-insta` | Snapshot tests. Good fit for the deterministic scanner ordering, the operation manifest, and CLI output. | installed |
| `cargo-machete` / `cargo-shear` / `cargo-udeps` | Unused-dependency detection across a 6-crate workspace. | installed |
| `cargo-mutants` | Mutation testing. Flux's `.gitignore` already ignores `mutants.out*`, so it was anticipated. Valuable for the invariant-heavy core (hardlink topology, resume, atomicity). | **NOT installed** — `cargo binstall -y cargo-mutants` |
| `cargo-zigbuild` | Cross-compiling to Linux/macOS targets from this Windows box. | installed |
| `criterion` (crate, not a binary) | Spec §3 asks for `benches/` and the CLI has a `flux benchmark` command. | add as dev-dep |

## Added for Flux

| Tool | Config file | What it does for us | Status |
|---|---|---|---|
| `actionlint` | — | Lints every GitHub Actions workflow: YAML syntax, `${{ }}` expressions against the context types, job and step references, action inputs. The workflows change often (the model check's per-scenario matrix, the extended tier's label and schedule triggers, Dependabot action bumps), and those mistakes otherwise surface only when CI runs. `just lint-workflows` locally; the `Workflow lint` job in `ci.yml` runs the pinned `rhysd/actionlint:1.7.12` image on every CI run. | 1.7.12 (`winget install rhysd.actionlint`) |
| `shellcheck` | — | actionlint runs every workflow `run:` step through it when it is on PATH, and silently skips that check when it is not (visible only with `actionlint -verbose`). The CI image bundles it. | 0.11.0 (`winget install koalaman.shellcheck`) |
| `gh` | — | GitHub CLI. The model checks run only on CI, so their results come back through `gh run watch`, `gh run view --log` and `gh run download` (the TLC log artifacts). Pull requests, labels such as `model-extended`, and Dependabot auto-merge are driven through it too. | (`winget install GitHub.cli`) |
| `java` | — | Runs the TLA+ tools in the pinned `tla2tools.jar` (Java 11 or later; CI uses Temurin 21). Locally it runs only the PlusCal translator and the SANY parser when `models/lockproto/algorithm.txt` changes. The TLC model checks themselves are too heavy for a development machine and run only on CI (owner ruling, 2026-09-13) — do not run `just model` locally. Not needed by `just check`. | Temurin 21 (`winget install EclipseAdoptium.Temurin.21.JDK`) |
| `tla2tools.jar` | — | The pinned TLA+ tools jar (TLC 2.19, SHA-256 checked by `models/lockproto/run.py`) that the local translation and SANY parse need; see `java`. `run.py` downloads it into `target/tla/` on first use. | fetched on demand |
| `python3` | — | Runs `models/lockproto/run.py` (the model-check runner) and its unit tests (`just model`, `just model-test`). Python 3.14 is what CI and the development machines use; `run.py` accepts 3.11 or later (`tomllib`). Not needed by `just check`. | 3.14 (`winget install Python.Python.3.14`) |

`actionlint` and `shellcheck` are winget installs, which Git Bash on this machine does not see until its PATH is reloaded, so there is no
pre-push hook for them; CI is the gate. A clean result is meaningful: a control workflow with an unquoted variable
(SC2086) and an undefined context property is reported (verified 2026-09-15).

## Deliberately not carried over

- `sqlx` / `cargo-sqlx` / `migrations/` — aishelter is Postgres-backed; Flux persists its
  operation workspace on the filesystem.
- `docker-compose.yml` / `.github/workflows/docker.yml` — no service to containerise.
- `pnpm` / `package.json` / `wrangler` / `pnpm-workspace.yaml` — aishelter's Cloudflare
  Workers + Lambda targets. Flux has no JS surface.
- `openapi.json` / `utoipa` / `prometheus-client` — no HTTP API, no metrics endpoint.
- `testcontainers` — no external services to stand up in tests.

## Gate commands (the contract)

```
just check          # fmt-check + clippy + test — the local gate
just check-linux    # the Linux leg: clippy + all tests, natively or through WSL
just check-mac      # the macOS leg: clippy only, cross-compiled; runs no tests
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
cargo test --doc    # nextest does not run doctests
cargo deny check
typos
just lint-workflows # actionlint (+ shellcheck over run: steps); CI job "Workflow lint"
```

## Keeping this file honest

Every tool named here that an agent or a fresh checkout has to *install* is also declared in
`.claude/recommended-tools.json`, which the SessionStart hook reads to report what is missing. The two are
machine-checked by `tests/dev_tooling.rs`: each tool in that JSON must appear in a table row above, and the
prose in `README.md` and `CONTRIBUTING.md` must still point at both files. The check runs one way only —
this file may document more than the JSON declares, because components that ship with the toolchain
(`rustfmt`, `clippy`), the installer that fetches the rest (`cargo-binstall`), and merely-installed
candidates all belong here but not in the file the hook acts on.
