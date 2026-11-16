"""Tests for packaging/updater-manifest.py.

Run from the repository root:

    python -m unittest discover -s packaging/tests -p "test_*.py"

The signatures here are shaped like shellune-sign's (base64 of a minisign .sig
text whose trusted comment names the version) but are not real signatures: the
script only checks that each one exists and names the right version. Real
signatures are checked by the updater crate (crates/update/tests/sign_cli.rs) and
by the dry run described in packaging/README.md.
"""

import base64
import contextlib
import importlib.util
import io
import json
import os
import shutil
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(os.path.dirname(HERE), "updater-manifest.py")

_spec = importlib.util.spec_from_file_location("updater_manifest", SCRIPT)
updater_manifest = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(updater_manifest)

VERSION = "1.2.3"
PLATFORM_KEYS = [
    "windows-x86_64",
    "windows-x86_64-portable",
    "darwin-aarch64",
    "linux-x86_64",
]


def fake_signature(file_name, version):
    text = (
        "untrusted comment: signature from shellune-sign\n"
        "RUSVMHqzPzrTBgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n"
        f"trusted comment: timestamp:1763251200\tfile:{file_name}\tversion:{version}\n"
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==\n"
    )
    return base64.b64encode(text.encode("utf-8")).decode("ascii")


class ManifestTest(unittest.TestCase):
    def setUp(self):
        self.dist = tempfile.mkdtemp(prefix="shellune-manifest-")
        self.addCleanup(shutil.rmtree, self.dist, ignore_errors=True)
        self.files = dict(updater_manifest.platform_files(VERSION))
        for file_name in self.files.values():
            self.write(file_name, b"release bytes of " + file_name.encode())
            self.write(file_name + ".sig", fake_signature(file_name, VERSION).encode())

    def write(self, name, content):
        with open(os.path.join(self.dist, name), "wb") as handle:
            handle.write(content)

    def run_script(self, *extra):
        output = os.path.join(self.dist, "latest.json")
        stderr = io.StringIO()
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(stderr):
            code = updater_manifest.main(
                [
                    "--version",
                    VERSION,
                    "--dist",
                    self.dist,
                    "--pub-date",
                    "2026-11-16T08:00:00Z",
                    "--output",
                    output,
                    *extra,
                ]
            )
        return code, output, stderr.getvalue()

    def assert_refused(self, *expected):
        code, output, stderr = self.run_script()
        self.assertEqual(code, 1, stderr)
        self.assertFalse(os.path.exists(output), "no manifest is written on failure")
        self.assertFalse(os.path.exists(output + ".tmp"))
        for text in expected:
            self.assertIn(text, stderr)
        return stderr

    def test_writes_every_platform_with_its_url_and_signature(self):
        notes = os.path.join(self.dist, "notes.md")
        with open(notes, "w", encoding="utf-8") as handle:
            handle.write("## 新功能\n\n- 自动更新\n")
        code, output, stderr = self.run_script("--notes-file", notes)
        self.assertEqual(code, 0, stderr)
        with open(output, encoding="utf-8") as handle:
            manifest = json.load(handle)

        self.assertEqual(manifest["version"], VERSION)
        self.assertEqual(manifest["notes"], "## 新功能\n\n- 自动更新")
        self.assertEqual(manifest["pub_date"], "2026-11-16T08:00:00Z")
        self.assertEqual(list(manifest["platforms"]), PLATFORM_KEYS)
        base = f"https://github.com/sakullla/shellune/releases/download/v{VERSION}"
        expected_files = {
            "windows-x86_64": f"Shellune-{VERSION}-setup.exe",
            "windows-x86_64-portable": f"Shellune-{VERSION}-windows-x86_64-portable.zip",
            "darwin-aarch64": f"Shellune-{VERSION}-macos-arm64.app.tar.gz",
            "linux-x86_64": f"shellune_{VERSION}_amd64.deb",
        }
        for key, file_name in expected_files.items():
            entry = manifest["platforms"][key]
            self.assertEqual(entry["url"], f"{base}/{file_name}")
            self.assertEqual(entry["signature"], fake_signature(file_name, VERSION))

    def test_a_leading_v_and_a_trailing_newline_in_the_signature_are_accepted(self):
        name = self.files["linux-x86_64"]
        self.write(name + ".sig", (fake_signature(name, VERSION) + "\r\n").encode())
        manifest = updater_manifest.build_manifest("v" + VERSION, self.dist, None, "now")
        self.assertEqual(manifest["version"], VERSION)
        self.assertEqual(
            manifest["platforms"]["linux-x86_64"]["signature"], fake_signature(name, VERSION)
        )
        self.assertIsNone(manifest["notes"])

    def test_the_base_url_can_be_replaced(self):
        manifest = updater_manifest.build_manifest(
            VERSION, self.dist, None, "now", "http://127.0.0.1:8000/files/"
        )
        self.assertEqual(
            manifest["platforms"]["darwin-aarch64"]["url"],
            f"http://127.0.0.1:8000/files/Shellune-{VERSION}-macos-arm64.app.tar.gz",
        )

    def test_each_missing_release_file_is_refused(self):
        for key, file_name in self.files.items():
            with self.subTest(key=key):
                path = os.path.join(self.dist, file_name)
                os.rename(path, path + ".away")
                try:
                    self.assert_refused(f"{key}:", file_name, "is missing")
                finally:
                    os.rename(path + ".away", path)

    def test_an_empty_release_file_is_refused(self):
        self.write(self.files["windows-x86_64"], b"")
        self.assert_refused("windows-x86_64:", "is empty")

    def test_a_missing_signature_is_refused(self):
        name = self.files["darwin-aarch64"]
        os.remove(os.path.join(self.dist, name + ".sig"))
        self.assert_refused("darwin-aarch64:", name + ".sig is missing")

    def test_an_empty_signature_is_refused(self):
        name = self.files["windows-x86_64-portable"]
        self.write(name + ".sig", b"  \n")
        self.assert_refused("windows-x86_64-portable:", name + ".sig is empty")

    def test_a_signature_for_another_version_is_refused(self):
        name = self.files["linux-x86_64"]
        self.write(name + ".sig", fake_signature(name, "1.2.2").encode())
        self.assert_refused("linux-x86_64:", "is for version 1.2.2, not 1.2.3")

    def test_a_signature_that_is_not_one_is_refused(self):
        name = self.files["windows-x86_64"]
        self.write(name + ".sig", b"not base64 at all!")
        self.assert_refused("windows-x86_64:", "is not a base64 signature")
        self.write(name + ".sig", base64.b64encode(b"hello"))
        self.assert_refused("does not hold a minisign signature")

    def test_every_problem_is_reported_at_once(self):
        os.remove(os.path.join(self.dist, self.files["windows-x86_64"]))
        os.remove(os.path.join(self.dist, self.files["linux-x86_64"] + ".sig"))
        self.assert_refused("windows-x86_64:", "linux-x86_64:")

    def test_an_invalid_version_is_refused(self):
        with self.assertRaises(updater_manifest.ManifestError):
            updater_manifest.build_manifest("1.2", self.dist, None, "now")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            code = updater_manifest.main(["--version", "next", "--dist", self.dist])
        self.assertEqual(code, 1)
        self.assertIn('"next" is not a semantic version', stderr.getvalue())
        self.assertFalse(os.path.exists(os.path.join(self.dist, "latest.json")))

    def test_a_missing_notes_file_is_refused(self):
        code, output, stderr = self.run_script(
            "--notes-file", os.path.join(self.dist, "absent.md")
        )
        self.assertEqual(code, 1)
        self.assertFalse(os.path.exists(output))
        self.assertIn("absent.md is missing", stderr)


if __name__ == "__main__":
    unittest.main()
