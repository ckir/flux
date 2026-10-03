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
5. **Panel round 1 changed one agreed detail, for the owner to confirm:** the harness times the runs itself rather than
   through `hyperfine` (see "Measuring" for why).

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

Every case copies a folder into a destination that does not exist yet: the files' contents, their modification times
and their permissions. Flux does that with its default options: it applies times and permissions unless told
otherwise (`crates/flux-cli/src/main.rs`, `--preserve-times` and `--preserve-permissions` only make a failure to apply
them fatal). Each comparator's command is chosen to do the same work and no less (see the registry), so a ratio never
compares a full copy with a contents-only one. Flux runs as a release build of `flux-cli` from the commit under test.

## The comparator registry

`benches/comparators.toml`. One table per comparator:

| Key | Meaning |
|---|---|
| `name` | shown on the charts |
| `os` | the runner OSes it runs on: any of `windows`, `linux`, `macos` |
| `command` | the copy command as an ARRAY of arguments, e.g. `["cp", "-Rp", "{src}/.", "{dst}"]`. Placeholders are substituted inside each argument, and the array is run directly, never through a shell, so a path with spaces stays one argument |
| `ok_exit_codes` | exit codes that mean success (robocopy's 1 means "files copied") |
| `version` | an argument array whose first output line is recorded as the version |
| `source` | `preinstalled`, or `download` with `url` and `sha256` |
| `note` | optional, shown on the trend page (a licence condition, a feature it lacks) |

Starting set:

| Name | OS | Command |
|---|---|---|
| `robocopy` | windows | `robocopy {src} {dst} /E /COPY:DAT /DCOPY:T /NJH /NJS /NFL /NDL /NP` (ok 0-7) |
| `cp` | linux, macos | `cp -Rp {src}/. {dst}` |
| `rsync` | linux, macos | `rsync -rtp {src}/ {dst}/` |

`/COPY:DAT` is robocopy's default (data, attributes, timestamps), written out so the work is explicit. `-p` makes `cp`
keep times and permissions; `-t` and `-p` do the same for `rsync`. None of them syncs to disk, and neither does Flux by
default (`--durability normal`).

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

The harness is `benches/publish/` in Python 3.14, standard library only, installed on the runners by
`actions/setup-python`. It times the runs itself (`time.perf_counter` around `subprocess.run` of the argument array).
It does not use `hyperfine`, for three reasons:
- hyperfine runs one command's repetitions back to back, while this design interleaves the tools;
- hyperfine's preparation step needs a shell;
- hyperfine stops on any non-zero exit, and robocopy's success code is 1.

For each case on a runner:

1. **Generate** the source tree once, from a fixed seed, under the runner's temporary directory.
2. **Warm up** (Windows only, which cannot drop its cache): one untimed copy by each tool.
3. **Time five rounds.** Round `r` runs Flux and every comparator for this OS, in the tool list rotated by `r`, so each
   tool goes first in some round. Before every timed copy, the harness:
   - deletes the destination;
   - on Linux, runs `sync` and then `sudo sh -c "echo 3 > /proc/sys/vm/drop_caches"`, as an argument array;
   - on macOS, runs `sync` and then `sudo purge`.

   Only the copy itself is inside the timed region.
4. **Check** every copy after it is timed, outside the timed region: the destination has the same relative paths as
   the source, and each file's SHA-256 matches. A tool whose copy is wrong, or whose exit code is not in its
   `ok_exit_codes`, is recorded as `failed` for that case and gets no ratio. The check runs on every round, so a tool
   cannot pass by copying correctly only once.
5. **Record** each tool's median of the five times, the spread (minimum and maximum), and the ratio of Flux's median to
   each comparator's.

### Result shapes

Each measure job writes one file, `result-<os>.json`, uploaded as its artifact:

```json
{
  "schema": 1,
  "commit": "<40-hex sha>",
  "date": "<UTC ISO 8601>",
  "os": "linux",
  "image": {"os": "<ImageOS>", "version": "<ImageVersion>"},
  "cache": "cold",
  "defender": null,
  "tools": {"flux": "<version line>", "cp": "<version line>"},
  "cases": {
    "small": {
      "times_s": {"flux": [0.0, 0.0, 0.0, 0.0, 0.0], "cp": [0.0, 0.0, 0.0, 0.0, 0.0]},
      "median_s": {"flux": 0.0, "cp": 0.0},
      "failed": [],
      "ratio": {"cp": 0.0}
    }
  }
}
```

`cache` is `cold` or `warm`, and `defender` is `true`, `false` or `null` (not Windows). `data.json` on `bench-data` is
`{"schema": 1, "runs": [ ... ]}`, where each run is one such object. A run is identified by (`commit`, `os`): a second
result for the same pair replaces the first, so a re-run never duplicates a point. `stability.json` is
`{"schema": 1, "pairs": {"<os>/<case>/<comparator>": {"cv": 0.0, "stable": true, "calibrated": "<date>", "image": "<ImageVersion>"}}}`.
A reader that meets a `schema` it does not know stops and draws nothing, rather than misreading the fields.

## Noise

- **Same-job ratios.** Both tools see the same VM at nearly the same moment, so much of the run-to-run swing cancels.
  That is a claim about these runners, so it is measured before anything is published (next bullet).
- **The stability gate.** A calibration workflow (`bench-calibrate.yml`) runs the benchmark job 10 times in parallel on
  each runner, at the same commit. For each (runner, case, comparator), it computes the coefficient of variation of the
  10 ratios. A pair at or below 3% is `stable`, which is enough to show a 10% regression. A pair above it is `unstable`
  and stays off the page until a later calibration passes. Results go to `stability.json`, and the page shows when each
  pair was last calibrated.
- **The gate is re-checked over time,** because 10 parallel jobs measure the spread between machines at one moment,
  not drift over weeks. Calibration runs weekly on a schedule, on `workflow_dispatch`, and from `bench.yml` whenever a
  runner reports an `ImageVersion` that `stability.json` has not seen. Between calibrations, the publish job computes
  the coefficient of variation of each pair's last 10 points at unchanged Flux source. A pair whose rolling figure
  exceeds 3% is demoted to `unstable` until the next calibration. Points at unchanged source are those whose commits
  changed no file under `crates/`, `Cargo.toml` or `Cargo.lock` since the previous point.
- **Cache.** Cold on Linux and macOS, warm on Windows; the page states which.
- **Windows is charted apart.** A warm cache, and Defender's on-access scan if it is active on the runner, measure a
  different profile from a cold Linux or macOS copy. The two are never on one chart. The dry run records whether
  Defender's real-time protection is on (`Get-MpComputerStatus`), and the page states it.
- **Runner image changes.** A change of `ImageVersion` between two points is drawn as a break in the line. A new image
  can shift every ratio, and that shift is not a Flux regression.

## Where the data lives and how the site is built

- **History:** the branch `bench-data`, which holds only `data.json` (every run, appended), `stability.json` and
  `latest.svg`. No bot commits to `main`; nothing expires.
- **The benchmark workflow** (`.github/workflows/bench.yml`):
  - **Trigger:** every push to `main`, plus `workflow_dispatch`.
  - **Measure jobs:** one per runner OS, with `contents: read`. Each uploads `result-<os>.json` as an artifact.
  - **Publish job:** the only job with `contents: write`. It runs after the measure jobs whether or not each succeeded
    (`if: always()`), and publishes the results that exist; a failed runner simply has no point for that commit. It
    appends the results to `data.json`, redraws
    `latest.svg`, and pushes to `bench-data`, rebasing and retrying if another push landed first. It runs under the
    concurrency group `bench`, which queues runs and never cancels one. It pushes with the job's `GITHUB_TOKEN`, and
    GitHub starts no workflow run for a push made with that token. That matters: `ci.yml` runs on every branch
    (`branches: ["**"]`), and a CI run on `bench-data`, which holds no code, would fail. Branch protection covers only
    `main` (measured: no rulesets; `main` has 9 required checks), so the push to `bench-data` is not blocked.
- **`latest.svg`** is drawn by the harness in Python: a small table-like image, one row per (runner, case, comparator)
  that is `stable`, with the ratio, the commit and the date.
- **The site:** `docs.yml` runs on its existing triggers and also on `workflow_run` of `bench.yml`. Before it uploads its
  Pages artifact, it asks whether `bench-data` exists (`git ls-remote --heads origin bench-data`). If it does not
  (before the first benchmark, or after the branch was deleted), the docs deploy without a `bench/` folder and the step
  says so in its log. If it does, it checks out `bench-data` into `target/doc/bench/` and copies in the trend page
  (`benches/publish/site/index.html`). One deploy serves both the docs and the benchmark pages. Pages here is built by
  Actions (`build_type: workflow`, measured), so a second deploying workflow would replace the docs, and a `gh-pages`
  branch would never be served.
- **The trend page** reads `data.json` and `stability.json` and draws with Chart.js, loaded from a CDN at a pinned
  version with a subresource-integrity hash. It shows only `stable` pairs, with the method stated at the top.

## Failure behaviour

- **A measure job fails:** that runner's point is missing from the run; the other runners' points still publish.
  Nothing published is overwritten.
- **`bench-data` is force-pushed back or deleted:** the history it held is lost from the page; the next run starts a
  new `data.json`. No other workflow fails.
- **The publish push fails after its retries:** the run fails visibly in Actions; the data branch keeps its last state.
- **`bench-data` is unreachable at deploy:** `docs.yml` deploys the docs without a `bench/` folder. The README image then
  shows as broken until the next good deploy. Visitors see a missing image, never wrong numbers.

## Testing

- **The harness's own logic** has unit tests in Python, run in CI:
  - registry parsing and validation: a missing placeholder, an unknown OS, a `download` entry with no `sha256`, a
    `command` that is not an array;
  - the rotation (each tool first in some round);
  - the copy check: a destination with a missing file, an extra file, or one wrong byte fails;
  - the exit-code rule;
  - the ratio and median arithmetic;
  - the coefficient of variation, the gate and the rolling demotion;
  - the `data.json` replace-by-(`commit`, `os`) rule;
  - an unknown `schema`;
  - the image-version break;
  - the `latest.svg` content (stable rows only).
- **A dry run** of the benchmark workflow on a branch, with tiny cases, writing to a scratch branch instead of
  `bench-data`, before the first real run.
- **The first calibration** runs before the README links anything. Its outcome decides which pairs appear.

## Not in scope

- Local-machine numbers, including the 2026-10-02 probe. They stay in `TODO.md`.
- `flux benchmark` (the CLI command in spec §4) and criterion micro-benchmarks. `benches/copy.rs` stays a placeholder.
- Alerting on a regression. The chart makes one visible; failing a build on it is a later decision.
- Refreshing the rest of the stale README ("pre-implementation").

## Stand-downs

- DISCARDED-BELOW-FLOOR (panel r1): a CDN outage leaves the trend page blank. The README's image is served by Pages,
  not the CDN, so the README is unaffected, and the page shows no numbers rather than wrong ones.
- DISCARDED-BELOW-FLOOR (panel r1): the publish job racing itself. It runs in the concurrency group `bench`, which queues
  runs, and its push rebases and retries.
- REJECTED (panel r1): "Python 3.14 does not exist yet." Python 3.14 was released in October 2025, and
  `actions/setup-python` installs it.
