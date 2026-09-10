# TODO

Near-term work. Release-level scope lives in [ROADMAP.md](ROADMAP.md).

## Open decisions

- [ ] **Final dependency selection** (spec §67). `[workspace.dependencies]` pins
      versions for the candidate crates but no member crate depends on any of them
      yet. Each must be judged on performance, correctness, portability,
      maintenance, licensing and API stability before adoption.
- [ ] **Directory walker.** Spec §67 lists `jwalk`, but crates.io currently ships
      it described as "Use `dua-core` instead" and it has not moved since 0.9.0.
      Decide between `walkdir`, a maintained parallel walker, or our own — the
      deterministic-ordering requirement (spec §7) may force a custom one anyway.
- [ ] **Persistent state format** for the topology store and operation manifest
      (spec §17, §19). Must scale past RAM and survive a crash mid-write.
- [ ] **Model-check the lock protocol before implementing it** (spec §96.1, §99,
      §240.3, §240.5, §259.6, and the claim/`COMMIT` ordering of §241.5 and
      §182). The V16 adversarial review stopped at its six-round cap still finding
      defects there each round, and round 6's fixes went unreviewed. Model two
      recoverers, two `--break-lock` takeovers, a stalled prior owner, and a plain
      run (for example with TLA+, or `loom`/`shuttle` against the Rust
      implementation) and check that at most one operation ever owns a target.
- [ ] Repository housekeeping: enable GitHub private vulnerability reporting (see
      [SECURITY.md](SECURITY.md)), and decide whether `main` gets branch protection.

## Phase 2 — portable copy (next)

- [ ] `flux-fs`: define the portable filesystem trait surface
- [ ] `flux-platform`: Linux / macOS / Windows implementations behind it
- [ ] `flux-core`: single-file copy with metadata preservation
- [ ] `flux-core`: recursive directory copy
- [ ] `flux-core`: error taxonomy and mapping (spec §68.1 lists error mapping as
      a required unit test)
- [ ] `flux-core`: statistics collection
- [ ] `flux-cli`: wire `flux copy` to the above
- [ ] Integration tests: single file, directory, nested directory, multiple
      sources, zero-byte files, Unicode, spaces, newlines (spec §68.2)

## Scaffolding follow-ups

- [ ] Install `cargo-mutants` (`cargo binstall -y cargo-mutants`) — it is the one
      tool in `.claude/recommended-tools.json` not yet present on the dev box
- [ ] Run `lefthook install` in each clone (or add it to a bootstrap recipe)
- [ ] Replace the placeholder `benches/copy.rs` once there is a pipeline to measure
- [ ] Replace the placeholder test in `tests/integration/mod.rs` with the first
      real case from spec §68.2
