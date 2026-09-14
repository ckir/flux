# Justfile for Flux

# Default: run the local gate
default:
    just check

# Build the workspace
build:
    cargo build --workspace

# Run all tests
test:
    cargo nextest run --workspace --no-tests=pass
    cargo test --doc --workspace

# Run tests without stopping at the first failure
test-verbose:
    cargo nextest run --workspace --no-tests=pass --no-fail-fast

# Clippy, warnings are errors
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Format check
fmt-check:
    cargo fmt --check

# Format all code
fmt:
    cargo fmt --all

# Spell check
typos:
    typos

# Dependency advisories, licences, bans and sources
deny:
    cargo deny check

# The local gate: fmt + clippy + typos + test
check: fmt-check clippy typos test

# Background watcher
watch:
    bacon

# Benchmarks
bench:
    cargo bench

# Build the docs
doc:
    cargo doc --workspace --no-deps --open

# Install the git hooks (once per clone)
hooks:
    lefthook install

# Generate the changelog
changelog:
    git-cliff --output CHANGELOG.md

# --- Lock-protocol model check (docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md)
# Needs Java 11+ and Python 3.11+; not part of `just check`. See models/lockproto/README.md.

python := env_var_or_default("PYTHON", "python3")

# Run the model check: every scenario, or one (`just model selftest`), optionally one platform
# (`just model recovery posix`)
model scenario="" platform="":
    {{python}} models/lockproto/run.py {{ if scenario == "" { "" } else { "--scenario " + scenario } }} {{ if platform == "" { "" } else { "--platform " + platform } }}

# Unit tests of the model runner (Python only, no Java)
model-test:
    {{python}} -W error -m unittest discover -s models/lockproto -p "test_*.py"

# Rewrite trace.toml's unit hashes; runs the whole model check first and stops if it fails
model-stamp:
    {{python}} models/lockproto/run.py
    cargo test --test model_stamp -- --ignored rewrite_unit_hashes --exact

# Mutation testing over the engine
mutants:
    cargo mutants --package flux-core

# Clean build artifacts
clean:
    cargo clean

# Release: bump every crate in lockstep, tag, commit
# Usage: just release <patch|minor|major>
release VERSION_BUMP:
    cargo release {{VERSION_BUMP}} --workspace --execute
