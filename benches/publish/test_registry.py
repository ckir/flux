"""Tests for registry.py: the comparator registry's format and its rules."""

from __future__ import annotations

import sys
import textwrap
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import registry  # noqa: E402

GOOD = """
[[comparator]]
name = "cp"
os = ["linux", "macos"]
command = ["cp", "-Rp", "{src}/.", "{dst}"]
source = "preinstalled"
"""


def entry(**fields: str) -> str:
    """One comparator table: GOOD's fields, with `fields` replaced or added (values are TOML literals)."""
    base = {
        "name": '"t"',
        "os": '["linux"]',
        "command": '["t", "{src}", "{dst}"]',
        "source": '"preinstalled"',
    }
    base.update(fields)
    return "[[comparator]]\n" + "".join(f"{k} = {v}\n" for k, v in base.items())


class RegistryTests(unittest.TestCase):
    def test_the_committed_registry_parses_to_the_starting_set(self) -> None:
        comps = registry.parse((HERE.parent / "comparators.toml").read_text("utf-8"))
        self.assertEqual([c.name for c in comps], ["robocopy", "cp", "rsync"])
        self.assertEqual([c.name for c in registry.for_os(comps, "windows")], ["robocopy"])
        self.assertEqual([c.name for c in registry.for_os(comps, "linux")], ["cp", "rsync"])
        robocopy = comps[0]
        self.assertEqual(robocopy.ok_exit_codes, frozenset(range(8)))

    def test_a_good_entry_parses_with_its_defaults(self) -> None:
        (cp,) = registry.parse(GOOD)
        self.assertEqual(cp.os, ("linux", "macos"))
        self.assertEqual(cp.ok_exit_codes, frozenset({0}))
        self.assertIsNone(cp.version)
        self.assertIsNone(cp.note)

    def test_placeholders_are_replaced_inside_each_argument_and_never_joined(self) -> None:
        (cp,) = registry.parse(GOOD)
        self.assertEqual(
            cp.argv("/tmp/a b", "/tmp/c d"),
            ["cp", "-Rp", "/tmp/a b/.", "/tmp/c d"],
        )

    def test_a_command_without_both_placeholders_is_refused(self) -> None:
        for command in ('["t", "{src}"]', '["t", "{dst}"]', '["t"]'):
            with self.subTest(command=command), self.assertRaisesRegex(registry.RegistryError, "src"):
                registry.parse(entry(command=command))

    def test_a_command_that_is_not_an_array_is_refused(self) -> None:
        with self.assertRaisesRegex(registry.RegistryError, "array"):
            registry.parse(entry(command='"t {src} {dst}"'))

    def test_an_unknown_os_is_refused(self) -> None:
        with self.assertRaisesRegex(registry.RegistryError, "unknown os"):
            registry.parse(entry(os='["linux", "freebsd"]'))

    def test_a_download_needs_an_https_url_and_a_sha256(self) -> None:
        sha = '"' + "a" * 64 + '"'
        with self.assertRaisesRegex(registry.RegistryError, "sha256"):
            registry.parse(entry(source='"download"', url='"https://example.com/t"'))
        with self.assertRaisesRegex(registry.RegistryError, "https"):
            registry.parse(entry(source='"download"', url='"http://example.com/t"', sha256=sha))
        (t,) = registry.parse(entry(source='"download"', url='"https://example.com/t"', sha256=sha))
        self.assertEqual(t.source, "download")

    def test_a_preinstalled_entry_carries_no_url(self) -> None:
        with self.assertRaisesRegex(registry.RegistryError, "preinstalled"):
            registry.parse(entry(url='"https://example.com/t"'))

    def test_flux_and_duplicates_and_unknown_keys_are_refused(self) -> None:
        with self.assertRaisesRegex(registry.RegistryError, "flux"):
            registry.parse(entry(name='"flux"'))
        with self.assertRaisesRegex(registry.RegistryError, "twice"):
            registry.parse(entry() + entry())
        with self.assertRaisesRegex(registry.RegistryError, "unknown keys"):
            registry.parse(entry(speed='"fast"'))

    def test_a_name_must_be_lowercase_letters_digits_dashes_or_underscores(self) -> None:
        # Test audit G7.
        for name in ('"Cp"', '"c p"', '"-cp"', '"cp!"', '""'):
            with self.subTest(name=name), self.assertRaisesRegex(registry.RegistryError, "name"):
                registry.parse(entry(name=name))
        (ok,) = registry.parse(entry(name='"fast_copy-2"'))
        self.assertEqual(ok.name, "fast_copy-2")

    def test_bad_exit_codes_are_refused(self) -> None:
        for codes in ("[]", '["0"]', "[true]"):
            with self.subTest(codes=codes), self.assertRaisesRegex(registry.RegistryError, "ok_exit_codes"):
                registry.parse(entry(ok_exit_codes=codes))

    def test_a_file_that_is_not_toml_is_refused(self) -> None:
        with self.assertRaisesRegex(registry.RegistryError, "TOML"):
            registry.parse(textwrap.dedent("[[comparator]\n"))


if __name__ == "__main__":
    unittest.main()
