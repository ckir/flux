# Flux Pages Site Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish a dark "full neon" landing page at the Pages root, move rustdoc to `/api/`, and keep every old rustdoc HTML URL working through generated redirect stubs.

**Architecture:** One stdlib-Python script, `pages/assemble.py`, with two modes. `assemble` moves `target/doc` into `target/doc/api/`, writes the crate-list page and one stub per old `.html` path, and overlays the hand-written `site/`. `verify` runs last, after the bench step, and checks the result against a manifest recorded before the move. `docs.yml`, the PR "Docs build" job and a `just` recipe all run the same script.

**Tech Stack:** Python 3.14 stdlib (`unittest`, no new dependency), hand-written HTML/CSS/JS, GitHub Actions, `just`.

**Spec:** `docs/superpowers/specs/2026-10-23-pages-site-design.md` (committed 8e0fd82). Read it first; section names below ("Old URLs", "Visual direction", "Calls made by the author") refer to it.

## Global Constraints

- Every link to something inside the site is relative (`bench/`, `api/flux_core/index.html`), never `/bench/`.
- Links to unpublished repo files are absolute `https://github.com/ckir/flux/blob/main/<file>`: `ROADMAP.md`, `FLUX_FULL_UPDATED_SPEC_V16.md`, `LICENSE`.
- Release links are exactly `https://github.com/ckir/flux/releases/latest/download/<asset>` for `flux-x86_64-unknown-linux-gnu.tar.gz`, `flux-aarch64-unknown-linux-gnu.tar.gz`, `flux-x86_64-apple-darwin.tar.gz`, `flux-aarch64-apple-darwin.tar.gz`, `flux-x86_64-pc-windows-msvc.zip`, plus a plain `https://github.com/ckir/flux/releases/latest` link (`.github/workflows/release.yml:33-47,74,80`).
- No external fonts, CDN scripts or stylesheets; system font stack; no light theme; no client-side GitHub API calls.
- No figure and no unconditional "faster"/"outperforms" anywhere on the page; speed sentences are conditional ("below 1.0 on the chart, Flux is faster") or point at the chart.
- Palette: void `#05060b`, cyan `#00f0ff`, violet `#7000ff`, blue `#3b82f6`. Canvas animation stops under `prefers-reduced-motion: reduce` and pauses in a hidden tab; the page works with JavaScript off.
- `docs` build job keeps `contents: read`; `deploy` keeps `pages: write` + `id-token: write`; no new required CI check context (extend the existing "Docs build" job, `.github/workflows/ci.yml:66-76`).
- Python files carry the licence header of `header.txt` (checked by `just license` for `*.py`); tests run with `python -W error -m unittest discover -s pages -p "test_*.py"`, as `ci.yml:144` runs `benches/publish`.
- Do not push and do not open a PR (`just pr` arms auto-merge); the owner reviews first.

## Review Focus

Inputs the spec implies and no task's happy path exercises, most likely first. Each has a named test in the owning task.

1. `assemble` run a second time on a tree that already has `api/` (a local re-run): must refuse, not nest `api/api/` (Task 1, `test_refuses_when_api_exists`).
2. A `site/` file whose name collides with a rustdoc top-level name (`help.html`, `flux_core`, `api`, `bench`): must fail before anything is moved (Task 1, `test_site_name_collisions_fail_before_any_move`).
3. An `.html` path with a character outside `[A-Za-z0-9._/-]`: must stop the build, never reach a stub (Task 1, `test_unsafe_html_path_fails_before_any_move`).
4. Pages built with no `bench-data` branch: `verify` passes, and the page's chart and trend-link wording is true with `bench/` absent (Task 2, `test_verify_passes_without_bench`; Task 4, `test_chart_has_object_fallback`).
5. A later step dropping or overwriting a moved file, a stub, or the landing page: `verify` must name it (Task 2, one mutation test per condition).

---

### Task 1: `pages/assemble.py` — manifest, stubs, `assemble`

**Files:**
- Create: `pages/assemble.py`, `pages/test_assemble.py`, `pages/api-index.html`
- Reference (do not edit): `.github/workflows/docs.yml:43-90`

**Interfaces:**
- Produces (all in `pages/assemble.py`):
  - `CANONICAL_BASE = "https://ckir.github.io/flux/api/"`, `SAFE_PATH = re.compile(r"[A-Za-z0-9._/-]+")`, `RESERVED_SITE_NAMES = frozenset({"api", "bench"})`
  - `class AssembleError(Exception)`
  - `def rel_target(rel_path: str) -> str` — `"../" * rel_path.count("/") + "api/" + rel_path` (not `len(Path.parents)`, which is one too many)
  - `def stub_html(rel_path: str) -> str`
  - `def list_files(doc: Path) -> list[str]` — sorted POSIX paths relative to `doc`, every file, `.lock` excluded
  - `def assemble(doc: Path, site: Path, template: Path, work: Path) -> None`
  - `def main(argv: list[str] | None = None) -> int` — subcommand `assemble` with `--doc` (default `target/doc`), `--site` (`site`), `--template` (`pages/api-index.html`), `--work` (`target/pages-work`); `AssembleError` prints `error: <message>` to stderr and returns 1
- Test helper in `pages/test_assemble.py`: `make_doc(root: Path) -> Path` builds a fake rustdoc tree: `help.html`, `crates.js`, `.lock`, `flux_core/index.html`, `flux_core/struct.X.html`, `static.files/a.css`, `search.index/s.js`, `src/flux_core/lib.rs.html`; `make_site(root: Path) -> Path` builds `index.html`, `style.css`, `flux-field.js`.

- [ ] **Step 1: Extract the crate-list template.** `sed -n '44,89p' .github/workflows/docs.yml | sed 's/^ \{10\}//' > pages/api-index.html` (the heredoc body without the 10-space YAML indent). Check: first line is `<!DOCTYPE html>`, last is `</html>`, `diff <(sed -n '44,89p' .github/workflows/docs.yml | sed 's/^ \{10\}//') pages/api-index.html` is empty.

- [ ] **Step 2: Write the failing tests** in `pages/test_assemble.py` (licence header, `sys.path` insert as in `benches/publish/test_svg.py:1-20`):

```python
class StubTests(unittest.TestCase):
    def test_depth_relative_targets(self):
        self.assertEqual(rel_target("help.html"), "api/help.html")
        self.assertEqual(rel_target("flux_core/struct.X.html"), "../api/flux_core/struct.X.html")
        self.assertEqual(rel_target("src/flux_core/lib.rs.html"), "../../api/src/flux_core/lib.rs.html")

    def test_stub_carries_refresh_canonical_and_hash_preserving_script(self):
        s = stub_html("flux_core/struct.X.html")
        self.assertIn('<meta http-equiv="refresh" content="0; url=../api/flux_core/struct.X.html">', s)
        self.assertIn('<link rel="canonical" href="https://ckir.github.io/flux/api/flux_core/struct.X.html">', s)
        self.assertIn('location.replace("../api/flux_core/struct.X.html" + location.search + location.hash)', s)

    def test_stub_escapes_hostile_characters(self):  # defence in depth below the whitelist
        s = stub_html('a"b</script>.html')
        self.assertNotIn('</script>.html', s)
        self.assertNotIn('"b<', s)

class AssembleTests(unittest.TestCase):  # each test in its own tempfile.TemporaryDirectory
    def test_moves_every_file_and_stubs_every_html_path(self): ...
    def test_root_is_landing_page_and_api_index_is_template(self): ...
    def test_lock_file_stays_put_and_is_not_in_api(self): ...
    def test_non_html_files_get_no_stub(self): ...
    def test_manifest_is_written_outside_doc(self): ...
    def test_refuses_when_api_exists(self): ...
    def test_site_name_collisions_fail_before_any_move(self): ...   # help.html, flux_core, api, bench; doc tree unchanged
    def test_site_must_be_flat_and_have_index(self): ...
    def test_unsafe_html_path_fails_before_any_move(self): ...      # doc/'we<ird>.html'; doc tree unchanged
    def test_work_inside_doc_is_refused(self): ...
```

Assertions: after `assemble`, every manifest path exists at `doc/api/<path>`; each `.html` manifest path is a file at its old path whose text equals `stub_html(path)`; `doc/index.html` bytes equal `site/index.html`; `doc/api/index.html` bytes equal the template; `doc/.lock` exists and `doc/api/.lock` does not; `doc/crates.js` and `doc/static.files` do not exist; `work/manifest.json` is the JSON list of `list_files` taken before the move. "Fail before any move" means the pre-call and post-call `list_files(doc)` are equal and `doc/api` does not exist.

- [ ] **Step 3: Run to verify they fail:** `python -W error -m unittest discover -s pages -p "test_*.py"`. Expected: import error / FAIL (`assemble` module missing).

- [ ] **Step 4: Implement** `pages/assemble.py` (with the licence header, as every `*.py` needs) and the signatures above. Order inside `assemble`, all checks before any mutation: (1) `doc` exists, `doc/api` does not, `work` is not inside `doc`; (2) manifest = `list_files(doc)`; every `.html` path matches `SAFE_PATH`; (3) `site` has no subdirectory, has `index.html`, and no entry whose name is in `RESERVED_SITE_NAMES` or equals a top-level entry of `doc`; (4) write `work/manifest.json`; (5) move every top-level entry of `doc` except `.lock` into `work/staging`, then `shutil.move` the staging directory to `doc/api`; (6) copy the template to `doc/api/index.html`; (7) write each stub (create parents); (8) copy each `site` file to `doc`. `stub_html` uses `html.escape(..., quote=True)` for attributes and `json.dumps` with `<`, `>`, `&` replaced by `\u003c`, `\u003e`, `\u0026` for the script string. Stub shape: `<!DOCTYPE html>`, `<meta charset>`, `<title>Redirecting</title>`, the refresh meta, the canonical link, the script, and a `<body><p>Moved to <a href=REL>REL</a>.</p></body>` for no-JS fallbacks.

- [ ] **Step 5: Run to verify they pass:** same command. Expected: `OK`, no warnings.

- [ ] **Step 6: Commit:** `git add pages && git commit -m "feat(pages): assemble the site, move rustdoc to api/ and write redirect stubs"`.

---

### Task 2: `verify` mode

**Files:**
- Modify: `pages/assemble.py`, `pages/test_assemble.py`

**Interfaces:**
- Consumes: Task 1's `list_files`, `stub_html`, `main`, `make_doc`, `make_site`, and `work/manifest.json`.
- Produces: `def verify(doc: Path, site: Path, work: Path) -> list[str]` — problem strings, empty means pass; `main` gains subcommand `verify` (`--doc`, `--site`, `--work`, same defaults), prints each problem to stderr and returns 1, or prints `verify: ok (<N> files, <M> stubs)` and returns 0.

- [ ] **Step 1: Write the failing tests** (class `VerifyTests`; helper `assembled()` runs `make_doc`, `make_site`, `assemble`). Each mutation test applies exactly one change to an assembled tree and asserts the substring in a problem:

```python
def test_clean_tree_passes(self):                 self.assertEqual(verify(doc, site, work), [])
def test_verify_passes_without_bench(self):       # no doc/bench at all -> []
def test_missing_moved_file_is_named(self):       # unlink doc/api/search.index/s.js -> "missing: api/search.index/s.js"
def test_missing_stub_is_named(self):             # unlink doc/help.html -> "bad stub: help.html"
def test_overwritten_stub_is_named(self):         # write other text to doc/flux_core/index.html -> "bad stub: flux_core/index.html"
def test_missing_api_index_is_named(self):        # -> "api/index.html"
def test_missing_static_files_is_named(self):     # rmtree doc/api/static.files -> "api/static.files"
def test_bench_under_api_is_named(self):          # mkdir doc/api/bench -> "api/bench"
def test_changed_landing_page_is_named(self):     # overwrite doc/index.html -> "landing page"
def test_missing_manifest_is_reported(self):      # delete work/manifest.json -> a problem mentioning "manifest"
def test_cli_exit_codes(self):                    # main(["verify", ...]) returns 0 on clean, 1 after one mutation
```

- [ ] **Step 2: Run to verify they fail:** same unittest command. Expected: FAIL (`verify` missing).

- [ ] **Step 3: Implement `verify`.** Checks, all collected (no early return) so one run names every offender: manifest readable; for every manifest path `doc/api/<path>` is a file (`missing: api/<path>`); for every `.html` manifest path the file at `doc/<path>` equals `stub_html(path)` (`bad stub: <path>`); `doc/api/index.html` is a file; `doc/api/static.files` is a directory; `doc/api/bench` does not exist; `doc/index.html` bytes equal `site/index.html` bytes (`landing page: ...`). Comparing to a regenerated stub, not to "a stub exists", is what makes the check non-circular.

- [ ] **Step 4: Run to verify they pass.** Expected: `OK`.

- [ ] **Step 5: Commit:** `git commit -am "feat(pages): verify the assembled site against the pre-move manifest"`.

---

### Task 3: wiring — `docs.yml`, `ci.yml`, `justfile`, `.gitignore`

**Files:**
- Modify: `.github/workflows/docs.yml:39-90` (delete), `.github/workflows/docs.yml` (add two steps), `.github/workflows/ci.yml:66-76`, `justfile`, `.gitignore`

**Interfaces:**
- Consumes: the `assemble` and `verify` CLIs with their defaults (`target/doc`, `site`, `pages/api-index.html`, `target/pages-work`).
- Produces: recipes `pages-test` and `pages-preview` in `justfile`.

- [ ] **Step 1: `docs.yml`.** Delete the whole step "Generate workspace index" (`docs.yml:39-90`; its content now lives in `pages/api-index.html`). After "Build docs" (`docs.yml:30-37`) insert `actions/setup-python@v7` with `python-version: "3.14"` (as `ci.yml:141-143`) and a step "Assemble the site" running `python pages/assemble.py assemble`. After "Add the benchmark pages" (`docs.yml:92-104`) and before `upload-pages-artifact` (`docs.yml:111`) insert a step "Verify the site" running `python pages/assemble.py verify`. Add a short comment on each step saying why it sits there (assemble before bench so `bench/` lands at the root; verify after bench so a later step cannot break the result unseen), in the file's existing comment style. Do not touch `permissions` or the bench step's body.

- [ ] **Step 2: `ci.yml`.** In the `doc` job (`ci.yml:66-76`), after the `cargo doc` step add: `actions/setup-python@v7` (3.14), `python -W error -m unittest discover -s pages -p "test_*.py"`, `python pages/assemble.py assemble`, `python pages/assemble.py verify`. No new job, so no new required check context.

- [ ] **Step 3: `justfile`.** Add `pages-test` (`{{python}} -W error -m unittest discover -s pages -p "test_*.py"`) and `pages-preview` (`rm -rf target/doc target/pages-work`; the same `cargo doc` command as `doc-check` without `RUSTDOCFLAGS`; assemble; verify; an `@echo` saying `bench/` exists only in CI so the chart card shows its fallback text locally; `{{python}} -m http.server 8000 --directory target/doc`). Place them beside `doc-check`; use the existing `python` variable (`justfile:160`). Do not add them to `check`.

- [ ] **Step 4: `.gitignore`.** Add `.superpowers/` under the "Local agent/tool state" block (next to `.serena/`, `.clavity/`).

- [ ] **Step 5: Verify.** Run `actionlint` (expected: no output), `just pages-test` (expected: `OK`), `typos` (expected: no output; add real domain words to `_typos.toml` only if flagged), and `git ls-files '*.py' | xargs addlicense -f header.txt -check` after `git add pages` (expected: no output).

- [ ] **Step 6: Commit:** `git add -A .github justfile .gitignore pages && git commit -m "ci(pages): run the site assembly and its checks in docs.yml, the PR docs job and just"`.

---

### Task 4: the landing page — `site/`

**Files:**
- Create: `site/index.html`, `site/style.css`, `site/flux-field.js`, `pages/test_site.py`

**Interfaces:**
- Consumes: Task 1's overlay (flat files only, `index.html` required).
- Produces: the page `assemble` copies to the Pages root. No other task depends on its internals.

- [ ] **Step 1: Write the failing tests** in `pages/test_site.py` (reads `site/index.html`, `style.css`, `flux-field.js` relative to the repo root; header as above):

```python
def test_site_dir_is_flat_and_has_only_expected_files(self)      # no subdirs; every entry in {index.html, style.css, flux-field.js} or an image
def test_no_external_resources(self)                              # no src=/href= to http(s) on <script>/<link>/<img>; no @import or url(http in css
def test_internal_links_are_relative(self)                        # no href="/ or src="/ ; contains "bench/" and "api/flux_core/index.html"
def test_five_release_links_and_latest_link_present(self)         # the six exact URLs from Global Constraints
def test_repo_file_links_are_absolute_blob_urls(self)             # ROADMAP.md, FLUX_FULL_UPDATED_SPEC_V16.md, LICENSE under https://github.com/ckir/flux/blob/main/
def test_quickstart_commands_present(self)                        # "./flux copy ./photos /mnt/backup", "./flux copy --resume ./photos /mnt/backup", ".\\flux.exe copy .\\photos D:\\backup"
def test_chart_has_object_fallback(self)                          # <object data="bench/latest.svg" type="image/svg+xml" with width and height attributes and child fallback text
def test_correctness_line_precedes_chart(self)                    # "correctness before performance" (case-insensitive) occurs before the <object>
def test_no_unconditional_or_numeric_speed_claims(self)           # no "outperform"; no regex \d+(\.\d+)?\s*[x×] ; no "\d+%\s*faster"
def test_licence_and_status_stated(self)                          # "PolyForm Noncommercial" and "early implementation" present
def test_install_lines_and_unsigned_note_present(self)            # "tar -xzf", ("unzip" or "Expand-Archive"), "Gatekeeper", "SmartScreen"
def test_canvas_script_honours_reduced_motion_and_hidden_tab(self)# flux-field.js contains matchMedia( with "prefers-reduced-motion", an addEventListener("visibilitychange" and a document.hidden read
```

- [ ] **Step 2: Run to verify they fail:** `python -W error -m unittest discover -s pages -p "test_site.py"`. Expected: FAIL (files missing).

- [ ] **Step 3: Build the page** to satisfy the tests and the spec's "Visual direction". Content, in order: the correctness line (`README.md:26`); an H1 pointing at the chart ("see how Flux compares with `cp` and `rsync`", no number); the framed white chart card holding the `<object>` (explicit size 720 wide, `overflow-x: auto` card, fallback text true whether or not `bench/` exists, a caption "below 1.0, Flux is faster", a note that a pair appears only once its ratio is steady and that Windows is compared with `robocopy`); the five-row download table plus the plain releases link and the licence next to the call to action; one extract-and-run line per OS family and a one-line unsigned-binary note; the two-line quickstart (Linux/macOS as `./flux`, Windows as `.\flux.exe` with Windows paths); secondary sections (goals and the pipeline taken from `README.md`, not invented; a trend link to `bench/` worded to be true while `bench/` does not exist; "API docs" listing the six crates under `api/`: flux-core, flux-fs, flux-platform, flux-hash, flux-ui, flux-cli; links to the roadmap and spec); a plain status block (`flux copy` and `flux cleanup` work, `flux verify` and `flux benchmark` do not exist yet; no commercial-use implication). `style.css`: the palette, grid, one corner glow, glass cards, gradient button, system font stack, responsive down to phone width with no page-level horizontal scroll, body text at WCAG AA contrast. `flux-field.js`: a few dozen lines on a `<canvas>` behind the hero; draws one still frame or nothing under `prefers-reduced-motion: reduce`; stops its loop on `visibilitychange` when hidden.

- [ ] **Step 4: Run to verify they pass:** `just pages-test`. Expected: `OK` (Task 1–2 tests plus these).

- [ ] **Step 5: Commit:** `git add site pages/test_site.py && git commit -m "feat(site): the Flux landing page"`.

---

### Task 5: end to end on the real rustdoc output

**Files:** none created; findings go in the PR body text drafted in the final step.

- [ ] **Step 1: Build and assemble for real:** `rm -rf target/doc target/pages-work && RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --document-private-items && python3 pages/assemble.py assemble && python3 pages/assemble.py verify`. Expected: `verify: ok (N files, M stubs)`; record N and M (the spec measured 1,330 files and 959 `.html` on an earlier build; expect the same order of magnitude, not those exact numbers).

- [ ] **Step 2: Old URLs on a real server.** `python3 -m http.server 18080 --directory target/doc &`, then with `curl -s` check, quoting the output: `/flux_core/index.html` contains `location.replace` and `api/flux_core/index.html`; `/api/flux_core/index.html` returns 200; `/src/flux_core/lib.rs.html` (or the first file under `target/doc/api/src/`) is a stub; `/help.html` is a stub; `/index.html` is the landing page; `/api/` serves the crate list; `/flux_core` (no slash) returns a 301. Stop the server.

- [ ] **Step 3: Both chart states in a real browser** (the chart's two states are the spec's required manual check). Serve once as is (no `bench/`): the card shows its fallback text, no broken image, the trend-link wording is true. Then `mkdir target/doc/bench && cp` any small SVG to `target/doc/bench/latest.svg`, reload, confirm the chart appears in the white card and scrolls horizontally inside it at phone width (browser devtools, 360 px) with no page-level horizontal scroll. Remove the stub `bench/`.

- [ ] **Step 4: The rest of the spec's "Manual acceptance" list**, each recorded as pass/fail with the browser used: no animation with `prefers-reduced-motion: reduce` and it pauses in a hidden tab; page readable with JavaScript off; text contrast at WCAG AA (devtools contrast check on body and muted text); an old deep link such as `/flux_core/struct.<Item>.html#<anchor>` (pick a real item) lands on the new page with the anchor kept.

- [ ] **Step 5: Whole gate:** `just pages-test`, `typos`, `actionlint`, `git ls-files '*.py' | xargs addlicense -f header.txt -check`, and `just doc-check`. Expected: all clean. Then `git status --short` shows only intended changes (`.superpowers/` is now ignored).

- [ ] **Step 6: Stop, do not push.** Draft the PR description (what changed, the Step 1 counts, the Step 2 curl output, the Step 3–4 checklist results, and that the first deploy happens on merge) into the final message to the owner. Do not run `just pr`.

---

## Self-review

- **Spec coverage:** landing page and visual direction (Task 4); `/api/` move, stubs, relative targets, canonical, hash-preserving script, escaping, fail-closed whitelist, manifest, refusal when `api/` exists, staging outside `target/doc`, landing page byte-identical, verify conditions (Tasks 1–2); crate-list template moved out of the heredoc, `docs.yml` order, PR job, `just` recipe, `.gitignore` (Task 3); manual acceptance list and both chart states (Task 5). Left to the builder and not planned: a link-check CI step (the spec marks it optional).
- **Not in the spec's words, decided here:** the script lives in `pages/` (not `site/`, which is copied to the root, and not `benches/publish/`, which is the benchmark harness); `verify` compares each stub to a regenerated stub (stronger than "points at"); the unit tests of the generator run in the existing "Docs build" job, so no new check context.
- **Type consistency:** `list_files`, `stub_html`, `rel_target`, `assemble`, `verify`, `main`, `AssembleError`, `make_doc`, `make_site` are named identically wherever they recur.
- **Proportion:** the plan holds signatures, test names with their assertions and the decisions the spec leaves open; function bodies are left to the implementer.
