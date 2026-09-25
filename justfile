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
