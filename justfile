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

# Lint the GitHub Actions workflows (actionlint, with shellcheck over `run:` steps when it is on PATH;
# without shellcheck that check is silently skipped). CI runs the same in the Workflow lint job.
lint-workflows:
    actionlint

# Dependency advisories, licences, bans and sources
deny:
    cargo deny check

# The local gate: fmt + clippy + typos + test
check: fmt-check clippy typos test

# --- Cross-platform legs of the local gate ---------------------------------
# `just check` compiles and tests for THIS machine only, so a `#[cfg]` mistake for
# another platform cannot fail it (two such breaks reached CI in PR #37). These two
# close as much of that as one Windows machine can:
#   check-linux  the full Linux gate (clippy + every test) in WSL: catches Linux
#                compile AND runtime differences.
#   check-mac    macOS clippy by cross-compiling: catches macOS compile and lint
#                breaks ONLY. It runs no test, so a macOS runtime difference (APFS
#                enforcing UTF-8 file names, for one) is still caught only by CI.
# On a Linux or macOS machine each runs natively instead.

wsl_distro := env_var_or_default("FLUX_WSL_DISTRO", "Ubuntu-26.04")
mac_target := "aarch64-apple-darwin"

# Linux leg of the gate: clippy + every test on Linux (through WSL on Windows)
check-linux:
    #!/usr/bin/env sh
    set -eu
    gate='cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace --no-tests=pass --no-fail-fast'
    case "$(uname -s)" in
        Linux) exec sh -c "$gate" ;;
        MINGW*|MSYS*|CYGWIN*) ;;
        *) echo "check-linux: needs Linux, or Windows with WSL; this is $(uname -s). CI runs the Linux leg." >&2; exit 1 ;;
    esac
    # Git for Windows rewrites arguments that look like POSIX paths; nothing below
    # may be rewritten.
    export MSYS_NO_PATHCONV=1
    if ! wsl.exe -d "{{wsl_distro}}" -- true >/dev/null 2>&1; then
        echo "check-linux: the WSL distribution '{{wsl_distro}}' is not available." >&2
        echo "  install it:  wsl --install -d {{wsl_distro}}   (or set FLUX_WSL_DISTRO to one you have)" >&2
        exit 1
    fi
    # A Linux-side target directory: sharing the Windows one would make each side
    # rebuild everything after the other ran. One per worktree, so two do not collide.
    # --exec, NOT `--`: without it wsl.exe joins the arguments into one string and
    # runs it through the distribution's default shell, which expands every `$var`
    # in the script (all unset there) before bash sees it. MEASURED:
    # `-- bash -c 'A=1; echo "[$A]"'` prints `[]`; `--exec bash -c ...` prints `[1]`.
    wsl.exe -d "{{wsl_distro}}" --cd "$(pwd -W)" --exec bash -lc '
        for tool in cargo cargo-nextest; do
            command -v "$tool" >/dev/null || {
                echo "check-linux: $tool is not on the WSL login PATH of {{wsl_distro}}." >&2
                echo "  install: rustup (https://rustup.rs), then: cargo install --locked cargo-nextest" >&2
                exit 1
            }
        done
        export CARGO_TARGET_DIR="$HOME/.cache/flux-target/$(basename "$PWD")"
        '"$gate"

# macOS leg of the gate: macOS clippy by cross-compiling (runs no tests; see above)
check-mac:
    #!/usr/bin/env sh
    set -eu
    if [ "$(uname -s)" = "Darwin" ]; then
        exec cargo clippy --workspace --all-targets -- -D warnings
    fi
    if ! rustup target list --installed | grep -qx "{{mac_target}}"; then
        echo "check-mac: the Rust target {{mac_target}} is not installed." >&2
        echo "  install it:  rustup target add {{mac_target}}" >&2
        exit 1
    fi
    # The root crate is left out: criterion's `alloca` build script compiles C for
    # the target and needs a macOS C toolchain. The member crates are where the
    # #[cfg] code lives.
    cargo clippy --workspace --exclude flux --all-targets --target {{mac_target}} -- -D warnings

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

# --- Lock-protocol model check (docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md)
# Needs Java 11+ and Python 3.11+; not part of `just check`. See models/lockproto/README.md.

python := env_var_or_default("PYTHON", "python3")

# Run the model check: every scenario, or one (`just model selftest`), optionally one platform
# (`just model recovery posix`), optionally one CI job (`just model breaklock-remote posix posix-plain`;
# see `run.py --list-jobs`, which is what the workflow's matrix is built from)
model scenario="" platform="" job="":
    {{python}} models/lockproto/run.py {{ if scenario == "" { "" } else { "--scenario " + scenario } }} {{ if platform == "" { "" } else { "--platform " + platform } }} {{ if job == "" { "" } else { "--job " + job } }}

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

# --- Outward actions -------------------------------------------------------
# These PUSH and OPEN PULL REQUESTS. Each gates itself first, so the checks a
# maintainer would otherwise have to remember are the ones they cannot skip.

# Push the current branch, refusing everything that should not be pushed
push:
    #!/usr/bin/env sh
    set -eu
    branch=$(git rev-parse --abbrev-ref HEAD)
    if [ "$branch" = "main" ] || [ "$branch" = "HEAD" ]; then
        echo "refusing: on '$branch'. Make a branch first." >&2
        exit 1
    fi
    if [ -n "$(git status --porcelain)" ]; then
        echo "refusing: the working tree is dirty. Commit or stash first." >&2
        git status --short >&2
        exit 1
    fi
    if [ -z "$(git log origin/main..HEAD --oneline)" ]; then
        echo "refusing: '$branch' has no commits origin/main does not have." >&2
        exit 1
    fi
    # NOT hypothetical: `git worktree add -b <branch> <path> origin/main` leaves
    # the NEW branch tracking origin/main, so a bare `git push` from a fresh
    # worktree pushes to MAIN. That has happened twice. This recipe always
    # pushes with `-u origin <branch>`, which cannot hit that case and repoints
    # the branch on the way past -- but it still says so, because a branch that
    # tracks origin/main will bite any OTHER bare push too.
    if [ "$(git rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null || true)" = "origin/main" ]; then
        echo "note: '$branch' tracks origin/main, which 'git worktree add -b' does by"
        echo "      default. Pushing with -u repoints it. A BARE 'git push' here would"
        echo "      have pushed to MAIN."
    fi
    just check
    git push -u origin "$branch"

# Push as above, then open the PR: just pr "title" [body.md]
pr title body="":
    #!/usr/bin/env sh
    set -eu
    # The body is a FILE on purpose. A PR body is the argument for the change and
    # is worth writing; one generated from commit subjects produces something
    # nobody reads. Omitting it opens the browser on the compose page, which is
    # the right default for a human and the wrong one for a script.
    #
    # VALIDATED BEFORE THE PUSH, and it was not at first: checking after meant a
    # mistyped path left the branch pushed with no PR, which is the worst state
    # this recipe can end in. On Windows, pass a relative or a C:/-style path --
    # MSYS rewrites a leading-slash path, so '/tmp/b.md' arrives as
    # 'C:/Program Files/Git/tmp/b.md'.
    if [ -n "{{body}}" ] && [ ! -f "{{body}}" ]; then
        echo "refusing: body file '{{body}}' does not exist." >&2
        exit 1
    fi
    just push
    branch=$(git rev-parse --abbrev-ref HEAD)
    if [ -n "{{body}}" ]; then
        gh pr create --base main --head "$branch" --title "{{title}}" --body-file "{{body}}"
        # AUTO-MERGE: the PR merges itself, as a merge commit, once every required
        # check passes. Opt out per PR with `gh pr merge --disable-auto <n>`. It
        # does not update a branch that falls behind main -- merge main in and it
        # proceeds. A failure here is reported, not fatal: the PR already exists.
        gh pr merge "$branch" --auto --merge \
            || echo "warning: could not enable auto-merge; merge it by hand." >&2
    else
        # --web returns before the PR exists, so there is nothing to arm yet.
        gh pr create --base main --head "$branch" --title "{{title}}" --web
        echo "note: auto-merge is not enabled for a PR composed in the browser;" \
            "run 'gh pr merge --auto --merge' once it is open." >&2
    fi
