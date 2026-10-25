# Flux GitHub Pages site — design

Status: draft for approval. Date: 2026-10-23; revised 2026-10-25 (rustdoc moves to `/api/`, old URLs kept by
redirect stubs, visual direction chosen).

## Intent and why

Flux has a Pages deployment today (`.github/workflows/docs.yml`) but its root is a hand-written
"Flux Workspace Documentation" list of rustdoc links, aimed at contributors. A visitor who lands on
`https://ckir.github.io/flux/` learns nothing about what Flux is or how to get it.

The site's job, in the owner's words: the visitor should **see it beats `cp` and `rsync`**, and **try it**.
The way to try it is **download a binary from the latest GitHub release**.

Decided by the owner on 2026-10-25: the rustdoc output moves under `/api/`; the old rustdoc URLs keep working
through redirect stubs; the look follows the owner's mockup (`/home/user/Pictures/Flux/flux_github_pages_hero_showcase.html`,
outside the repository), "full neon" direction, dark only. The mockup supplies palette and effects only: its copy
("workflow & stream engine") does not describe Flux, and its Tailwind CDN and Google Fonts are not used.

## Goals

- A modern, responsive, dark-only landing page at the Pages root, in the "full neon" style (see "Visual direction").
- Every rustdoc HTML page that resolves today at the Pages root still reaches the same page after the move to
  `/api/` (redirect stubs, "Old URLs" below), and the build fails if a stub's target is missing.
- Hero: a headline that points at the chart (no figure, no unconditional "faster" — see the calls below),
  the live benchmark chart, and a call to action that leads to the download table, which sits directly below
  the hero (amended 2026-10-25 after the final review: the table is its own section, not inside the hero box).
- The first line states the correctness promise in one sentence ("correctness before performance",
  `README.md:26`), so the speed claim is never bare.
- A download table with one direct link per release target, and a two-line quickstart: `flux copy ./photos /mnt/backup`, then, labelled "interrupted? run it again with
  `--resume`", `flux copy --resume ./photos /mnt/backup`. No invented sample output. Measured 2026-10-23 on the
  release build: `--resume` with no prior operation prints "no prior operation; starting new" and copies normally
  (exit 0), so it is safe to show. (`flux cleanup --dry-run` right after a clean copy prints an empty table, so it
  is not used as the second line.) The snippet runs the extracted binary as `./flux` on Linux and macOS and
  `.\flux.exe` on Windows, or says to put it on `PATH`; a bare `flux` fails right after extraction. The Windows
  snippet uses Windows paths (for example `.\flux.exe copy .\photos D:\backup`), not `/mnt/backup`.
- Secondary sections, shorter than the hero: goals, the pipeline, a link to the benchmark trend page (`bench/`),
  an "API docs" section listing the six crate links under `api/`, and links to the roadmap and the spec on GitHub.
- State plainly: early implementation; `flux copy` and `flux cleanup` work; `flux verify` and
  `flux benchmark` do not exist yet; licence is PolyForm Noncommercial 1.0.0 (shown next to the download CTA).

## Non-goals

- No site generator, no Node/npm, no new CI toolchain.
- No client-side JavaScript calling the GitHub API (60 requests/hour/IP limit; fails offline).
- No user-agent sniffing to pick an asset (misidentifies architecture).
- No rewrite of rustdoc output, the benchmark pages, or the `bench-data` branch.
- No custom domain, analytics or cookie banner.
- No external fonts, CDN scripts or light theme. First-party JavaScript is allowed for the canvas animation and
  the stubs, never for fetching data.
- No server-side redirects (GitHub Pages cannot issue them). Redirect stubs return HTTP 200 and redirect in the
  browser.
- No stubs for non-HTML rustdoc files (`static.files/`, `search.index/`, `crates.js`, `trait.impl/`, `type.impl/`,
  `src-files.js`): nobody links to them, and a rustdoc page loaded from `/api/` fetches its own copies.

## Anti-goals (a failure even if it technically works)

- Any existing public URL breaking. `/bench/` and `/bench/latest.svg` (embedded in `README.md`) do not move.
  The rustdoc pages (`/flux_core/`, `/flux_fs/`, `/flux_platform/`, `/flux_hash/`, `/flux_ui/`, `/flux/`, and
  every page under them, source views `src/` included) move to `/api/` and their old URLs must still reach the
  same page.
- A speed claim the chart does not support at that moment, or hiding that the chart omits a pair.
- Implying `verify` or `benchmark` work, or that commercial use is permitted.

## Constraints

- GitHub Pages serves under `/flux/`, so every link to something inside the site is **relative** (`bench/`, not `/bench/`).
  Links to repository files that are not published (`ROADMAP.md`, the spec, `LICENSE`) are absolute
  `https://github.com/ckir/flux/blob/main/...` URLs.
- `latest.svg` is a text-only 720px SVG with a hardcoded white background (`benches/publish/svg.py:59`), so on a
  dark page it must sit in a framed light card. It can read "not yet calibrated on this image" or "too noisy"
  for a pair (`svg.py:44-46`) when a runner-image update resets calibration; the card must make that readable
  and the surrounding copy must not promise a number the chart is not showing.
- Release assets are exactly `flux-<target>.tar.gz` (Linux and macOS) and `flux-x86_64-pc-windows-msvc.zip`
  (`.github/workflows/release.yml`, five targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
  `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`). Links use GitHub's
  `https://github.com/ckir/flux/releases/latest/download/<asset>` redirect.
- The `docs` build job keeps `contents: read`; `deploy` keeps `pages: write` + `id-token: write`.
- `site/` holds flat files only (`index.html`, `style.css`, one small script for the canvas, and any image the page needs). It must not contain an
  entry named `api`, `bench`, or like any top-level entry of the rustdoc output: the copy step would overwrite
  rustdoc output, and `bench/` is written afterwards by the bench step. Measured on a local `cargo doc` build, those
  top-level names are the seven crate directories `flux`, `flux_cli`, `flux_core`, `flux_fs`, `flux_hash`,
  `flux_platform`, `flux_ui` (the CLI package has both a library and the `flux` binary), plus `src`, `static.files`,
  `search.index`, `trait.impl`, `type.impl`, `crates.js`, `src-files.js`, `settings.html` and `help.html`. That
  list is an illustration, not the rule: `assemble` computes the set from the actual tree and fails on any
  `site/` entry with a colliding name, so a rustdoc release that adds a top-level name cannot slip past a
  hand-kept list.
- Step order in the `build` job, each step finishing before the next: `rm -rf target/doc target/pages-work`
  (added 2026-10-25: `Swatinem/rust-cache` saves `target/` after removing only the FILES under `target/doc`, so the
  emptied directory tree of `api/` is restored on the next run on the same cache key and `assemble` would refuse
  with "api exists"; source `src/cleanup.ts:10-34`, `src/save.ts:50`); `cargo doc`; then `assemble`:
  record the manifest, move everything in `target/doc` into `target/doc/api/` (via a staging directory, since the
  destination is inside the source), write `api/index.html` (the crate list), write the stubs, overlay `site/` onto
  `target/doc`; "Add the benchmark pages" (still last, so `bench/` lands at `target/doc/bench/`, never under
  `api/`); verify; upload `target/doc`. Moving after the bench step would put `bench/` at `/api/bench/` and break
  `README.md:17` and `README.md:22`.
- `latest.svg` is a fixed 720px-wide SVG (`svg.py:57`); it has no `viewBox` (`svg.py:57-58`), so CSS cannot scale it down: on a
  phone it sits in a card that scrolls it horizontally (`overflow-x: auto`), so the page itself has no horizontal
  scroll. If an `<object>` is used (see "No chart"), it is sized explicitly (an `<object>` defaults to 300x150).
- The site is deployed in the same artifact as rustdoc (`target/doc`), in the existing `build` job. As today, a
  failing `cargo doc` (`RUSTDOCFLAGS=-D warnings`) blocks the whole deploy; the site is not split into its own job.
- `flux copy` takes `SOURCE... DEST` (`crates/flux-cli/src/main.rs:65`), so the example is
  `flux copy ./photos /mnt/backup`.

## Old URLs (redirect stubs)

After rustdoc is moved to `target/doc/api/`, CI walks the moved tree and writes one stub at the old path of every
`.html` file in it (measured on a local `cargo doc` build: 1,330 files, 959 of them `.html`, 0 non-HTML files under
`src/`; about 290 KB of stubs). Rules:

- The stub redirects with a **relative** path computed from its depth (`../` repeated, then `api/<same path>`), never
  `/flux/api/...`, so a local preview and forks work (the Constraints above require relative links).
- Each stub carries a `<meta http-equiv="refresh" content="0; url=...">`, an absolute
  `<link rel="canonical" href="https://ckir.github.io/flux/api/<path>">`, and a script doing
  `location.replace(target + location.search + location.hash)`, so a deep link like `#method.scan` keeps its anchor.
- The root `index.html` is never stubbed: the old root page is replaced by the landing page (the old workspace
  index is now `/api/`).
- The overlay of `site/` runs after the stubs, so it would silently replace a stub that shares its name; the
  forbidden names in Constraints make that unreachable, and the verify step below catches it if it happens anyway.
- Paths are escaped where they are written into the stub (HTML attribute and JavaScript string); the generator is
  tested on a fixture that includes an awkward file name. It also fails closed: a stub is never written for an
  `.html` path containing a character outside `[A-Za-z0-9._/-]` (measured: all 1,330 files in a local build conform),
  so a future rustdoc layout or item name that needs other characters stops the build with a message instead of
  reaching the stub's inline script.
- Stubs are regenerated from each build's tree. A rustdoc page that is removed from `main` later loses its stub as
  it loses its page; the stubs cover the pages that exist when the move ships, not pages that never existed.
- The generator records a **manifest** of every file under `target/doc` before the move (not only `.html`; the
  cargo lock file `.lock` is excluded). The manifest and the staging directory live outside `target/doc` (for
  example under `target/`), so neither can be moved into `api/` and published. `assemble` refuses to run, with a
  message, if `target/doc/api` already exists, so a local re-run cannot nest `api/api/`; the local recipe runs
  `cargo doc` fresh first. The **verify** step (run after the bench step, as the last step before upload) fails
  the build, naming every offender, unless: for every manifest path `api/<path>` exists; for every `.html`
  manifest path the file at `<path>` is a stub pointing at `api/<path>`; `api/index.html` and `api/static.files/`
  exist; `target/doc/api/bench` does not exist; and `target/doc/index.html` is byte-identical to `site/index.html`
  (that is the definition of "is the landing page"). Checking against the manifest, not against the stubs just
  written, is what stops the check from passing trivially when a later step drops or overwrites something, and
  covering every file, not only HTML, catches a move that loses `search.index/` or `trait.impl/`.
- The stubs are permanent: dropping them later is a separate owner decision.
- The canonical link's host (`https://ckir.github.io/flux`) is a constant in the script. A fork's stubs would name
  the upstream site as canonical; accepted, since a fork does not need search ranking.
- Non-HTML files are not stubbed (Non-goals).
- Language of the generator is the builder's call; `benches/publish/` already uses Python, so standard-library
  Python with no new dependency is the default.

## Visual direction

Owner choice: "full neon" (direction A of the 2026-10-25 mockups), dark only.

- Palette from the owner's mockup: void background `#05060b`, primary accent cyan `#00f0ff`, secondary violet
  `#7000ff`, tertiary blue `#3b82f6`; glass cards (translucent dark fill, thin cyan border, soft glow); a faint
  cyan grid; one violet glow in a corner; a glowing primary button filled from cyan to blue (amended 2026-10-25:
  dark label text on violet measures 3.0:1, below WCAG AA, so violet stays in the button's glow, not its fill).
- An animated "flux field" `<canvas>` behind the hero, a few dozen lines of vanilla JS in `site/`. It must not run
  when the visitor has `prefers-reduced-motion: reduce` (draw one still frame or nothing) and it pauses while the
  tab is hidden. The page is fully usable and readable with JavaScript off.
- System font stack for text and `ui-monospace` for code; no web fonts.
- The benchmark chart stays a white card on the dark page (its SVG has a hard-coded white background).
- Text contrast must stay readable on the dark background (body text at WCAG AA or better); glow is decoration,
  never the only carrier of meaning.
- The mockup's "Documentation Quickstart" and "Release Notes & Benchmarks" panels are not carried over: the first
  duplicates the quickstart, the second would claim content that does not exist.

## Approaches

1. **A. Hand-written static `site/` directory — chosen.** `docs.yml` copies it over the generated root index.
   No toolchain, no CI time, URLs untouched. Cost: text edits are manual; nothing checks links in CI unless added.
2. **B. Site generator (Astro/Starlight, Zola, mdBook).** Richer docs navigation, but adds Node or a binary to a
   workflow with neither, a dependency feed, and routing that could clash with rustdoc's `static.files/` and
   `search.index/`. Too heavy for a one-page site on an early-stage tool.
3. **C. Restyle the existing heredoc.** No new files, but cannot be previewed or linted. Rejected.
4. (Variant D, raised by agy, deferred.) A small Python script that generates the page, to stop release links
   and README text drifting. Add only if the page drifts.

agy-first consult (peer and I agree on A). The one disagreement was the hero. The peer recommended a
safety-first headline with the chart beside it; the owner chose, on my recommendation, to keep the benchmark in
the hero with a one-line correctness promise above it and an honest, framed chart. Verified against
`svg.py:44-46,59`, `stability.py:12-13`, `README.md:26`, `release.yml`.

Old-URL fork (2026-10-25), options considered: (A) one generated stub per old HTML page; (B) stubs for the six crate
index pages only, so deep links break; (C) a single `404.html` that rewrites old paths in JavaScript, which serves
HTTP 404 and fails without JavaScript. agy-first consult and I agreed on A (`[VERDICT: ALIGNED]`); the owner chose A.
agy's changes to my first sketch, each checked: relative targets instead of `/flux/api/...`, the `src/` pages are all
HTML (measured: 0 non-HTML files there) so they get stubs, the root `index.html` must be exempt, and the redirect
script must keep `location.hash`. agy also suggested adding a `404.html` catch-all on top of A; not adopted (it
would catch only URLs nobody links to).

## Decided

- Approach A; relative links only; static release-redirect links, no JS fetch.
- Hero order: correctness line, a headline that points at the chart, framed `bench/latest.svg` card, download
  table, quickstart.
- Download table: five rows (OS / architecture / asset link), plus a plain link to
  `https://github.com/ckir/flux/releases/latest` for the case where one matrix entry failed and its asset is missing
  (`release.yml` runs with `fail-fast: false`).
- Rustdoc moves to `/api/`; old rustdoc HTML URLs keep working through generated redirect stubs (option A of the
  old-URL fork). Visual direction A, dark only.

## Calls made by the author (not the owner — check these)

- **Verified:** `v0.18.0` is a published, non-draft, non-prerelease release carrying all five assets under the
  names above (`gh release view v0.18.0`, 2026-10-23). The five download links resolve today. No checksum asset
  exists, so the page offers none.
- **Rustdoc entry point:** the crate-list page that the "Generate workspace index" heredoc in `docs.yml:39-90`
  writes today is moved verbatim into a template file in the repository, and `assemble` writes it to
  `api/index.html`; the heredoc step is deleted from `docs.yml`. Reason: the PR "Docs build" job
  (`ci.yml:66-76`) runs only `cargo doc` and has no such step, so leaving the page to `docs.yml` would make `verify`
  fail on every PR; one script gives the PR job and the deploy job the same output. `/api/` is the crate list and
  `/` is the landing page. The landing page's "API docs" section keeps linking the six crates the old page linked
  (`api/flux_core/index.html` and so on); `cargo doc` emits a seventh directory, `flux_cli` (the CLI library), which
  the stubs cover because they are derived from the tree. `/api/` is the one new public URL.
- **Manual acceptance (no automated check exists for these; recorded in the PR by the builder, in a real
  browser):** the canvas does not animate with `prefers-reduced-motion: reduce` and pauses in a hidden tab; the page
  is usable with JavaScript off; body text meets WCAG AA contrast; a deep link such as an old
  `.../struct.X.html#method.y` keeps its anchor after the redirect; the page renders correctly with and without
  `bench/`.
- **Speed headline wording:** the headline asserts no number and no unconditional "faster". It points at the
  chart ("see how Flux compares with `cp` and `rsync`"). The chart caption carries "below 1.0, Flux is faster",
  and a note next to the chart says that a pair appears only once its ratio is steady. Windows is compared with
  `robocopy`, not `cp`/`rsync`; the note says so. Nothing can verify the claim at build time, so the page must
  never state a figure. The rule covers the whole page, not only the H1: any sentence about speed anywhere (sub-
  headline, badge, section text) is either conditional ("below 1.0 on the chart, Flux is faster") or points at the
  chart. "Outperforms" and similar unconditional wording is out everywhere.
- **No chart:** if `bench/latest.svg` is missing (no `bench-data` branch, which `docs.yml` already tolerates),
  the card must not show a broken-image icon. The page is static HTML, so the card text cannot change by state: it
  is written once and is true either way (for example "The chart is published by CI from the latest run of
  `main`; until the first benchmark has run there is nothing here yet"). Use an element that renders its own
  fallback content on a failed load (for example `<object data="bench/latest.svg" type="image/svg+xml">` with
  the fallback as its child) rather than a bare `<img>`. The builder must check both states in a real browser
  (page served with and without `bench/`). The headline stays qualitative, so nothing else on the page changes.
  The same holds for the "benchmark trend" link to `bench/`: the page is static, so its wording must be true when
  `bench/` does not exist yet (for example "published by CI after the first benchmark run"), and a dead link must
  not be presented as a working one.
- **`.nojekyll`:** not added. The artifact is deployed through `actions/upload-pages-artifact` and
  `actions/deploy-pages`, not a branch build, and none of the flat files starts with an underscore.
- **Link check:** recommended but left to the builder (a CI step or `just` recipe); the owner did not ask for it.
- **Install guidance:** below the download table, one extract-and-run line per OS family (tar for Linux and
  macOS, unzip for Windows) and a one-line note that the binaries are not code-signed, so macOS Gatekeeper or
  Windows SmartScreen may warn. The rest of the page does not grow with this.

- **Assembly is one script with two modes, tested on PRs (builder-check answer):** `assemble` (record the manifest,
  move, stubs, `site/` overlay) and `verify` (after the bench step), one script in the language per "Old URLs", run
  by a `just` recipe, by `docs.yml`, and by the existing PR "Docs build" job (`.github/workflows/ci.yml:66-76`
  already runs `cargo doc` on every branch push and PR; extra steps in it add no new required check context), so a
  broken assembly fails the PR instead of the deploy. Only `docs.yml` deploys. The script takes the doc directory as
  an argument so it can run on a fixture.
- **"Same page" means the target file exists (builder-check answer):** the verify step checks every stub target,
  plus `api/index.html` and `api/static.files/`. No headless browser or crawler: that would add a toolchain, and
  moving the whole rustdoc tree as one unit keeps its relative paths valid.
- **The mockup is reference only (builder-check answer):** the palette, effects and rules under "Visual
  direction" are the complete visual spec; the builder does not need the mockup file and need not match it.

## Left to the builder

- Visual design within "Visual direction": exact spacing, type scale, layout of sections, the hand-written CSS
  file, and the canvas animation's look. Dark only; no `prefers-color-scheme` variant.
- Adding `.superpowers/` (the brainstorming mockups directory) to `.gitignore`.
- Exact copy of the headline and section text, within the claims above.
- The mechanism that overlays `site/` onto `target/doc` without clobbering rustdoc output or `bench/`
  (the builder must keep the existing "Add the benchmark pages" step and its ordering working).
- Whether to add a link check (CI step or `just` recipe) for the static pages.
- A local preview recipe. `bench/` exists only in CI (`bench-data` is an orphan branch), so the recipe must say
  how the chart looks locally (a stub or a documented broken image), not leave it to surprise.

## Stand-downs

- DISCARDED-BELOW-FLOOR: that the stub's `location.replace(target + location.search + location.hash)` is an
  unvalidated reflector (panel round 2). Unreachable as a vulnerability because the redirect target is computed at
  build time from the file's own path (Old URLs, the redirect rule), never read from the URL; the stub forwards the
  visitor's own query and fragment to a same-origin page and interprets nothing.
- UNVERIFIED-ACCEPTED: the peer's claim that a zero-byte `.nojekyll` prevents "edge-case path filtering" on
  workflow deployments. Not reproduced; the file is not added (author's call, see above).
