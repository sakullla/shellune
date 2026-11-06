#!/usr/bin/env python3
"""Assemble Shellune.app from a built macOS binary.

Usage:
    python3 packaging/macos/make-app-bundle.py --binary <path to shellune> \
        [--version 0.1.0] [--out dist]

The script only lays out the bundle; it does not sign or notarise it. Signing needs
an Apple Developer ID certificate and is done in CI (see
.github/workflows/release.yml) or by hand:

    codesign --force --options runtime --sign "Developer ID Application: ..." Shellune.app
    xcrun notarytool submit Shellune-<version>-macos-arm64.zip --wait ...
    xcrun stapler staple Shellune.app

This file has never been executed: the verification machine is Windows, so the
macOS bundle is scaffolding and the release notes say it is unverified.
"""

import argparse
import os
import plistlib
import shutil
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

INFO = {
    "CFBundleName": "Shellune",
    "CFBundleDisplayName": "Shellune",
    "CFBundleIdentifier": "com.shellune.Shellune",
    "CFBundleExecutable": "shellune",
    "CFBundlePackageType": "APPL",
    "CFBundleShortVersionString": "{version}",
    "CFBundleVersion": "{version}",
    "LSMinimumSystemVersion": "13.0",
    "NSHighResolutionCapable": True,
}


def main():
    parser = argparse.ArgumentParser(description="assemble Shellune.app")
    parser.add_argument("--binary", required=True)
    parser.add_argument("--version", default="0.1.0")
    parser.add_argument("--out", default=os.path.join(REPO, "dist"))
    args = parser.parse_args()

    binary = os.path.abspath(args.binary)
    if not os.path.isfile(binary):
        sys.exit("binary not found: " + binary)

    app = os.path.join(os.path.abspath(args.out), "Shellune.app")
    if os.path.isdir(app):
        shutil.rmtree(app)
    macos = os.path.join(app, "Contents", "MacOS")
    resources = os.path.join(app, "Contents", "Resources")
    os.makedirs(macos)
    os.makedirs(resources)

    target = os.path.join(macos, "shellune")
    shutil.copy2(binary, target)
    os.chmod(target, 0o755)
    shutil.copy2(os.path.join(REPO, "LICENSE"), os.path.join(resources, "LICENSE"))

    info = {k: (args.version if v == "{version}" else v) for k, v in INFO.items()}
    with open(os.path.join(app, "Contents", "Info.plist"), "wb") as f:
        plistlib.dump(info, f)

    print("bundle: " + app)
    print("note:   unsigned and unnotarised")


if __name__ == "__main__":
    main()
