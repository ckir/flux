#!/usr/bin/env python3
"""Record TLC -tool output for test_run.py. Run once after changing the TLC pin; needs Java.

Each case writes testdata/<case>.out: the first line is 'exit=<status>', the rest is TLC's output.
"""

from __future__ import annotations

import re
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import run  # noqa: E402

# case name -> (module, cfg text, -continue?)
CASES = {
    "clean": ("Fixture", "SPECIFICATION Spec\nINVARIANT Bounded\n", True),
    "continue_two_invariants": ("Fixture", "SPECIFICATION Spec\nINVARIANTS NeverTwo NeverThree\n", True),
    "halt_invariant": ("Fixture", "SPECIFICATION Spec\nINVARIANT NeverThree\n", False),
    "initial_invariant": ("Fixture", "SPECIFICATION Spec\nINVARIANT NotZero\n", True),
    "deadlock": ("Fixture", "INIT Init\nNEXT Step\n", True),
    "temporal": ("Fixture", "SPECIFICATION Spec\nPROPERTY EventuallyFive\n", True),
    "temporal_and_invariant": ("Fixture", "SPECIFICATION Spec\nINVARIANT NeverTwo\nPROPERTY EventuallyFive\n", True),
    "eval_error": ("Fixture", "SPECIFICATION Spec\nINVARIANT EvalError\n", True),
    "config_error": ("Fixture", "SPECIFICATION Spec\nINVARIANT Missing\n", True),
    "parse_error": ("Broken", "INIT Init\nNEXT Next\n", True),
}

# Coverage-gate fixtures (design Section 4, Part B): real TLC output recorded with '-coverage 1',
# used by the coverage-parsing and label-ownership tests. Unlike CASES above, each of these runs
# against a module and .cfg that are committed in testdata/ (CoverageFixture.tla/.cfg) or in the
# model directory proper (Smoke.tla + configs/selftest-check.cfg), not an ephemeral temp file, so
# the case maps directly to (module, cfg path, working directory) instead of inline cfg text.
COVERAGE_CASES = {
    # case -> (module, cfg path, working directory, -continue?)
    "coverage": ("CoverageFixture", HERE / "CoverageFixture.cfg", HERE, True),
    "smoke_check_coverage": ("Smoke", HERE.parent / "configs" / "selftest-check.cfg", HERE.parent, True),
    "smoke_seeded_coverage": ("Smoke", HERE.parent / "configs" / "selftest-seeded-SEED_OVERSHOOT.cfg",
                              HERE.parent, False),
}

_BLOCK = re.compile(
    r"@!@!@STARTMSG 2201:0 @!@!@.*?@!@!@ENDMSG 2202 @!@!@\n", re.S)


def _extract_block(text: str) -> str:
    """Return the single coverage block (2201 start through 2202 end, inclusive) in TLC output."""
    match = _BLOCK.search(text)
    if not match:
        raise SystemExit("no coverage block found in output being spliced")
    return match.group(0)


def record_case(jar: Path, case: str, module: str, cfg_text: str, cont: bool) -> None:
    with tempfile.TemporaryDirectory() as tmp:
        cfg = Path(tmp) / f"{case}.cfg"
        cfg.write_text(cfg_text, encoding="utf-8")
        cmd = ["java", "-XX:+UseParallelGC", "-cp", str(jar), "tlc2.TLC", "-tool", "-workers", "1",
               "-metadir", str(Path(tmp) / f"states-{case}"), "-config", str(cfg)]
        if cont:
            cmd.append("-continue")
        cmd.append(module)
        proc = subprocess.run(cmd, cwd=HERE, capture_output=True, text=True)
        output = (proc.stdout + proc.stderr).replace("\r\n", "\n")
        (HERE / f"{case}.out").write_text(f"exit={proc.returncode}\n{output}", encoding="utf-8", newline="\n")
        print(f"{case}: exit {proc.returncode}")


def record_coverage_case(jar: Path, case: str, module: str, cfg: Path, cwd: Path, cont: bool) -> str:
    """Record one coverage fixture run against a committed module and .cfg; return its output text
    (without the 'exit=' prefix) so build_two_block_fixture() can splice pieces of it."""
    with tempfile.TemporaryDirectory() as tmp:
        cmd = ["java", "-XX:+UseParallelGC", "-cp", str(jar), "tlc2.TLC", "-tool", "-workers", "1",
               "-metadir", str(Path(tmp) / f"states-{case}"), "-config", str(cfg)]
        if cont:
            cmd.append("-continue")
        cmd.extend(["-coverage", "1", module])
        proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
        output = (proc.stdout + proc.stderr).replace("\r\n", "\n")
        (HERE / f"{case}.out").write_text(f"exit={proc.returncode}\n{output}", encoding="utf-8", newline="\n")
        print(f"{case}: exit {proc.returncode}")
        return output


def build_two_block_fixture(coverage_output: str, smoke_output: str) -> None:
    """coverage_two_blocks.out: a snapshot-then-final pair of coverage blocks, needed to test that
    parse_coverage() takes the LAST complete block. A real run only prints a second block after a
    full minute (design Section 4), which is impractical to record here, so this fixture is
    synthesized by splicing two REAL recorded blocks together: coverage.out's block stands in for
    an early one-minute snapshot, and smoke_check_coverage.out's block is the final one. Everything
    else in the file - the preamble and the closing Progress/Finished messages - is
    smoke_check_coverage.out's, unmodified, so the file still parses as one coherent -tool run; only
    the two coverage blocks it carries are not from the same run."""
    snapshot_block = _extract_block(coverage_output)
    final_block = _extract_block(smoke_output)
    before, _, after = smoke_output.partition(final_block)
    composite = before + snapshot_block + final_block + after
    (HERE / "coverage_two_blocks.out").write_text(f"exit=0\n{composite}", encoding="utf-8", newline="\n")
    print("coverage_two_blocks: built from coverage.out + smoke_check_coverage.out")


# coverage_long_terminator.out (design Section 4, parse_coverage's 2777 terminator fix) is NOT
# built by this script: TLC prints the block terminator as 2202 ("End of statistics.") on a short
# run, but switches to 2777 ("End of statistics (please note that for performance reasons large
# models are best checked with coverage and cost statistics disabled).") once a run has gone on for
# a few minutes, which the CASES/COVERAGE_CASES above do not run long enough to reach. This fixture
# was instead extracted from a real long run's recorded '-tool -coverage 1' log (a recovery scenario
# check, captured outside this repo while diagnosing the 2777 defect): its three coverage blocks -
# the second block (2201..2202, the one the pre-fix parser wrongly returned) and the last two blocks
# (2201..2777 each, the second of which is the true final block) - were kept as their real, unedited
# '@!@!@STARTMSG ...@!@!@ENDMSG...@!@!@' message frames (codes 2201, 2202, 2777, 2772, 2773, 2774);
# every 2221 cost sub-line was dropped to keep the file small, since parse_coverage() never reads
# 2221. No message text was invented. The extraction (for reference, should the source log ever need
# re-deriving):
#
#   MESSAGE = re.compile(r"@!@!@STARTMSG (\d+):(\d+) @!@!@\n(.*?)@!@!@ENDMSG \1 @!@!@", re.S)
#   KEEP = {"2201", "2202", "2777", "2772", "2773", "2774"}
#   def trimmed(block_text):
#       return "\n".join(m.group(0) for m in MESSAGE.finditer(block_text) if m.group(1) in KEEP) + "\n"
#   # block2/block3/block4 are the three '@!@!@STARTMSG 2201...' through terminator line spans of
#   # the source log, in order; block3 and block4 are the two 2777-terminated blocks.
#   text = "exit=0\n" + trimmed(block2) + trimmed(block3) + trimmed(block4)


def main() -> int:
    jar = run.ensure_jar()
    for case, (module, cfg_text, cont) in CASES.items():
        record_case(jar, case, module, cfg_text, cont)

    coverage_output = record_coverage_case(jar, "coverage", *COVERAGE_CASES["coverage"])
    smoke_output = record_coverage_case(jar, "smoke_check_coverage", *COVERAGE_CASES["smoke_check_coverage"])
    record_coverage_case(jar, "smoke_seeded_coverage", *COVERAGE_CASES["smoke_seeded_coverage"])
    build_two_block_fixture(coverage_output, smoke_output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
