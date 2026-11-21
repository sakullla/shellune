"""Check the app identity and usage text that macOS presents for LAN access."""

import plistlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "macos" / "make-app-bundle.py"


class MacBundleTests(unittest.TestCase):
    def test_local_and_release_have_distinct_identities_and_usage_descriptions(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            binary = root / "shellune"
            binary.write_bytes(b"fixture")
            for options, name, identifier in [
                ([], "Shellune", "com.shellune.Shellune"),
                (["--local"], "Shellune Local", "com.shellune.Shellune.local"),
            ]:
                with self.subTest(bundle=name):
                    subprocess.run(
                        [sys.executable, str(SCRIPT), "--binary", str(binary),
                         "--out", str(root), *options],
                        check=True, capture_output=True,
                    )
                    contents = root / (name + ".app") / "Contents"
                    info = plistlib.loads((contents / "Info.plist").read_bytes())
                    self.assertEqual(info["CFBundleIdentifier"], identifier)
                    self.assertEqual(info["CFBundleDisplayName"], name)
                    self.assertTrue(info["NSLocalNetworkUsageDescription"].strip())
                    strings = contents / "Resources" / "zh-Hans.lproj" / "InfoPlist.strings"
                    self.assertIn("NSLocalNetworkUsageDescription", strings.read_text(encoding="utf-8"))
                    self.assertTrue((contents / "MacOS" / info["CFBundleExecutable"]).is_file())
