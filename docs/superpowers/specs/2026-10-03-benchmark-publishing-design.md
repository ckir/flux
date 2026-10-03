# Publishing Flux's benchmarks to repository visitors

The README tells a visitor nothing about speed today. The only measurement is a manual local probe (`TODO.md`,
"Performance (preliminary speed probe, 2026-10-02)"), and `benches/copy.rs` is a criterion placeholder. This design
publishes how Flux compares with other copiers, and how that changes over time, from CI alone.

## Owner decisions

1. **Purpose:** both a current comparison and a trend over time.
2. **Source:** CI only. Every published number comes from a GitHub Actions run that anyone can repeat.
3. **Approach:** the design below, agreed with agy after an AGY-FIRST consult and two negotiation turns
   (`.clavity/seams/bench-publish-forks.md`, `-negotiate.md`, `-negotiate-r2.md`; replies under
   `.clavity/scratch/bench-publish/`). It replaces the owner's first lean (CI, then Graphviz, then a README
   template):
   - Graphviz lays out node-and-edge graphs; it does not draw bar or line charts.
   - Filling in a README template needs a bot commit to `main`, which branch protection blocks.
4. **Comparators are a registry,** open to more tools later (FastCopy, for example), not a fixed pair.

## What a visitor sees

- **In `README.md`:** a "Performance" section that shows `https://ckir.github.io/flux/bench/latest.svg` inline, then
  three lines of method, then a link to the trend page. The image is stamped with the commit and date it measured, so
  a stale image says how stale it is. Pages serves it with `Cache-Control: max-age=600` (measured), and GitHub's
  image proxy honours that, so the README lags a run by about ten minutes at most. No commit to `main` is needed to
  update it.
- **At `https://ckir.github.io/flux/bench/`:** one chart per (runner, case), with one line per comparator, plotting the
  ratio over commits. Each point shows the commit, the date, the runner image and the tool versions. Windows charts sit
  apart from Linux and macOS charts, under their own heading (see "Noise").

The number published is the **ratio**: Flux's median time divided by the comparator's median time, both measured
interleaved in the same job. Below 1.0, Flux is faster. Absolute times are kept in the data but never charted. Hosted
runners swing about 40% between runs (measured on this repository's own CI), so an absolute trend would chart noise.

## Cases

Sized for a hosted runner's disk, and written fresh into the runner's temporary directory by the harness, with a fixed
seed so every run copies the same bytes:

| Case | Content |
|---|---|
| `large` | one 1 GiB file |
| `small` | 5,000 files of 4 KiB in 50 folders |
| `mixed` | 500 files totalling 512 MiB, sizes spread from 4 KiB to 64 MiB, in a three-level tree |

Every case copies a folder into a destination that does not exist yet, contents only. Each tool's command does that
and nothing more (see the registry). Flux runs as a release build of `flux-cli` from the commit under test, with its
default options.

## The comparator registry

`benches/comparators.toml`. One table per comparator:

| Key | Meaning |
|---|---|
| `name` | shown on the charts |
| `os` | the runner OSes it runs on: any of `windows`, `linux`, `macos` |
| `command` | the copy command, with `{src}` and `{dst}` placeholders, run without a shell |
| `ok_exit_codes` | exit codes that mean success (robocopy's 1 means "files copied") |
| `version` | a command whose first output line is recorded as the version |
| `source` | `preinstalled`, or `download` with `url` and `sha256` |
| `note` | optional, shown on the trend page (a licence condition, a feature it lacks) |

Starting set:

| Name | OS | Command |
|---|---|---|
| `robocopy` | windows | `robocopy {src} {dst} /E /NJH /NJS /NFL /NDL /NP` (ok 0-7) |
| `cp` | linux, macos | `cp -R {src}/. {dst}` |
| `rsync` | linux, macos | `rsync -r {src}/ {dst}/` |

Flux itself is fixed, not a registry entry: `flux copy {src} {dst}`.

A command must copy bytes the way Flux does. A tool that clones blocks instead (a copy-on-write `cp` on APFS, for
example) finishes almost instantly and says nothing about copying. The dry run (see "Testing") checks each starting
entry on its runner's temporary filesystem: a comparator whose destination shares blocks with its source gets a
`note`, and its command is changed to force a real copy where the tool allows that.

A comparator that is missing, fails its version command, or fails its SHA-256 check is skipped for that run. The
trend page shows the gap, and Flux's own measurement is unaffected. A `download` entry is fetched with its pinned
`url` and checked against `sha256` before it runs. Adding FastCopy later is a new entry; whether its licence allows
CI use is decided then and recorded in its `note`.

## Measuring

The harness is `benches/publish/` in Python 3.14 (standard library only) and `hyperfine`, pinned by version and SHA-256
like a downloaded comparator. For each case:

1. Generate the source tree once.
2. For each tool on this OS, run `hyperfine` with `--runs 5`, a `--prepare` step that deletes the destination and, on
   Linux and macOS, drops the page cache (`sync; echo 3 | sudo tee /proc/sys/vm/drop_caches`; `sudo purge`), and
   `--warmup 1` on Windows, which cannot drop it. Tools run in an order rotated by case, so no tool always goes first.
3. Check every destination: file count and total bytes equal the source's. A tool whose copy is wrong is recorded as
   failed for that case, never timed.
4. Record each tool's median and spread, and the ratio of Flux's median to each comparator's.

The run also records the commit, the date, the runner OS and image version (`ImageOS`, `ImageVersion`), and each
tool's version.

## Noise

- **Same-job ratios.** Both tools see the same VM at nearly the same moment, so much of the run-to-run swing cancels.
  That is a claim about these runners, so it is measured before anything is published (next bullet).
- **The stability gate.** A calibration workflow (`workflow_dispatch`) runs the benchmark job 10 times in parallel on
  each runner. For each (runner, case, comparator), it computes the coefficient of variation of the 10 ratios. A pair
  at or below 3% is `stable`, which is enough to show a 10% regression. A pair above it is `unstable` and stays off the
  page until a later calibration passes. Calibration results are stored beside the data (`stability.json`), and the
  page shows when each pair was last calibrated.
- **Cache.** Cold on Linux and macOS, warm on Windows; the page states which.
- **Windows is charted apart.** A warm cache and Defender's on-access scan measure a different profile from a cold
  Linux or macOS copy. The two are never on one chart.
- **Runner image changes.** A change of `ImageVersion` between two points is drawn as a break in the line. A new image
  can shift every ratio, and that shift is not a Flux regression.

## Where the data lives and how the site is built

- **History:** the branch `bench-data`, which holds only `data.json` (every run, appended), `stability.json` and
  `latest.svg`. No bot commits to `main`; nothing expires.
- **The benchmark workflow** (`.github/workflows/bench.yml`):
  - **Trigger:** every push to `main`, plus `workflow_dispatch`.
  - **Measure jobs:** one per runner OS, with `contents: read`. Each uploads its results as an artifact.
  - **Publish job:** the only job with `contents: write`. It appends the results to `data.json`, redraws
    `latest.svg`, and pushes to `bench-data`, rebasing and retrying if another push landed first. It runs under the
    concurrency group `bench`, which queues runs and never cancels one.
- **`latest.svg`** is drawn by the harness in Python: a small table-like image, one row per (runner, case, comparator)
  that is `stable`, with the ratio, the commit and the date.
- **The site:** `docs.yml` runs on its existing triggers and also on `workflow_run` of `bench.yml`. Before it uploads its
  Pages artifact, it checks out `bench-data` into `target/doc/bench/` and copies in the trend page
  (`benches/publish/site/index.html`). One deploy serves both the docs and the benchmark pages. Pages here is built by
  Actions (`build_type: workflow`, measured), so a second deploying workflow would replace the docs, and a `gh-pages`
  branch would never be served.
- **The trend page** reads `data.json` and `stability.json` and draws with Chart.js, loaded from a CDN at a pinned
  version with a subresource-integrity hash. It shows only `stable` pairs, with the method stated at the top.

## Failure behaviour

- **A measure job fails:** that runner's point is missing from the run. Nothing published is overwritten.
- **The publish push fails after its retries:** the run fails visibly in Actions; the data branch keeps its last state.
- **`bench-data` is unreachable at deploy:** `docs.yml` deploys the docs without a `bench/` folder. The README image then
  shows as broken until the next good deploy. Visitors see a missing image, never wrong numbers.

## Testing

- **The harness's own logic** has unit tests in Python, run in CI: registry parsing and validation (a missing
  placeholder, an unknown OS, a `download` entry with no `sha256`), the ratio and median arithmetic, the coefficient of
  variation and the gate, the `data.json` append (no duplicate commit per runner), the image-version break, and the
  `latest.svg` content (stable rows only).
- **A dry run** of the benchmark workflow on a branch, with tiny cases, writing to a scratch branch instead of
  `bench-data`, before the first real run.
- **The first calibration** runs before the README links anything. Its outcome decides which pairs appear.

## Not in scope

- Local-machine numbers, including the 2026-10-02 probe. They stay in `TODO.md`.
- `flux benchmark` (the CLI command in spec §4) and criterion micro-benchmarks. `benches/copy.rs` stays a placeholder.
- Alerting on a regression. The chart makes one visible; failing a build on it is a later decision.
- Refreshing the rest of the stale README ("pre-implementation").
