#!/usr/bin/env python3
"""Record TLC -tool output for test_run.py. Run once after changing the TLC pin; needs Java.

Each case writes testdata/<case>.out: the first line is 'exit=<status>', the rest is TLC's output.
"""

from __future__ import annotations

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


def main() -> int:
    jar = run.ensure_jar()
    with tempfile.TemporaryDirectory() as tmp:
        for case, (module, cfg_text, cont) in CASES.items():
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
    return 0


if __name__ == "__main__":
    sys.exit(main())
