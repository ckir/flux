# Contributing to Flux

Thanks for your interest. This document covers the basics.

Flux is built against an authoritative engineering specification,
[`FLUX_FULL_UPDATED_SPEC_V15.md`](FLUX_FULL_UPDATED_SPEC_V15.md). **The spec is the
oracle.** If the code and the spec disagree, that is a bug in the code — or a
change that needs to be made to the spec first, deliberately. Please cite the
relevant section (e.g. "spec §13.1") in issues and pull requests.

## Getting started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/YOUR_USERNAME/flux.git`
3. Install the tooling (see below) and run `lefthook install`
4. Create a branch: `git checkout -b my-feature`
5. Make your changes
6. Run the gate: `just check`
7. Commit and push: `git commit -m "feat: my feature" && git push origin my-feature`
8. Open a pull request

## Development setup

Requires Rust 1.85+ (edition 2024). The toolchain is pinned by
`rust-toolchain.toml`.

```bash
# One-time: install the dev tools
cargo binstall -y cargo-nextest just lefthook cargo-deny typos-cli bacon git-cliff cargo-release cargo-mutants
lefthook install

# Everyday
just build      # cargo build --workspace
just check      # the gate: fmt-check + clippy + typos + test
just watch      # bacon, in the background, while you work
just test       # nextest + doctests
just bench      # criterion benchmarks
just doc        # build and open the API docs
```

Every tool, what it gates and which config file drives it is documented in
[`docs/dev-tooling.md`](docs/dev-tooling.md). The same list is machine-checked from
`.claude/recommended-tools.json`.

## The gate

`just check` is exactly what CI runs. It must be green before you open a PR:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
typos
cargo nextest run --workspace --no-tests=pass
cargo test --doc --workspace       # nextest does not run doctests
```

CI additionally runs `cargo deny check` and runs the test job on Linux, macOS
**and** Windows. Flux is a filesystem engine — hardlinks, symlinks, sparse files,
reflinks and path case-folding all behave differently per platform, so a change
that passes on one OS is not evidence it passes on the others.

## Code style

- Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- `cargo fmt --all` before committing
- Fix every clippy warning; the gate treats them as errors
- All public items get `///` documentation
- Cite the spec section in doc comments for anything implementing a normative rule

## Testing

Spec §68 defines the testing strategy. §68.1 lists the required unit tests
(path normalization, deterministic ordering, Windows case tie-breaking, file
identity, hardlink canonicalization, resume validation, …) and §68.2 lists the
required integration cases. New behaviour should land with the matching test from
that list, and platform-specific behaviour needs the test on the platform it
concerns.

## Commit messages

We follow [Conventional Commits](https://www.conventionalcommits.org/), because
`git-cliff` generates the changelog from them:

```
type(scope): description

[optional body]

[optional footer(s)]
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`,
`chore`, `revert`

Examples:
- `feat(core): canonicalize hardlink groups during ordered traversal`
- `fix(platform): preserve mtime on Windows when the destination is read-only`
- `docs: document the operation-lock heartbeat lifecycle`

## Pull request process

1. CI must be green on all platforms
2. Update documentation if you changed behaviour
3. Add tests for new functionality
4. Reference the spec section and any related issues

## Reporting bugs

Open an issue with:
- Steps to reproduce
- Expected behaviour (cite the spec section if it defines one)
- Actual behaviour
- Environment: OS and version, filesystem type on both source and destination,
  Rust version, `flux --version`

Filesystem type matters more than usual here — please include it.

For security issues, do **not** open a public issue; see [SECURITY.md](SECURITY.md).

## Releasing

Releases use `cargo release` and follow [Semantic Versioning](https://semver.org/).
All crates are versioned in lockstep.

```bash
just release patch    # bug fixes
just release minor    # new features
just release major    # breaking changes
```

Pushing the resulting `v*` tag triggers the cross-platform release build.

## Licence

By contributing, you agree that your contributions will be licensed under the
[PolyForm Noncommercial License 1.0.0](LICENSE).
