#!/usr/bin/env python3
"""Assemble Shellune.app from a built macOS binary.

Usage:
    python3 packaging/macos/make-app-bundle.py --binary <path to shellune> \
        [--version 0.4.19] [--out dist]

The script only lays out the bundle; it does not sign it. Releases are signed in CI
with a fixed self-signed certificate (see .github/workflows/release.yml and
packaging/README.md), or by hand:

    codesign --force --options runtime --sign "<identity>" Shellune.app

Use --local for an independently identified development build.
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
    "CFBundleDevelopmentRegion": "en",
    "NSLocalNetworkUsageDescription": "Connect to the hosts you select on your local network for terminal and remote desktop sessions.",
    "CFBundleIconFile": "shellune.icns",
    "CFBundlePackageType": "APPL",
    "CFBundleShortVersionString": "{version}",
    "CFBundleVersion": "{version}",
    "LSMinimumSystemVersion": "13.0",
    "NSHighResolutionCapable": True,
}


def main():
    parser = argparse.ArgumentParser(description="assemble Shellune.app")
    parser.add_argument("--binary", required=True)
    parser.add_argument("--version", default="0.4.19")
    parser.add_argument("--out", default=os.path.join(REPO, "dist"))
    parser.add_argument("--local", action="store_true", help="use the local development app name and identifier")
    args = parser.parse_args()

    binary = os.path.abspath(args.binary)
    if not os.path.isfile(binary):
        sys.exit("binary not found: " + binary)

    name = "Shellune Local" if args.local else "Shellune"
    app = os.path.join(os.path.abspath(args.out), name + ".app")
    if os.path.isdir(app):
        shutil.rmtree(app)
    macos = os.path.join(app, "Contents", "MacOS")
    resources = os.path.join(app, "Contents", "Resources")
    os.makedirs(macos)
    os.makedirs(resources)
    shutil.copy2(os.path.join(REPO, "assets", "icons", "shellune.icns"), resources)

    target = os.path.join(macos, "shellune")
    shutil.copy2(binary, target)
    os.chmod(target, 0o755)
    shutil.copy2(os.path.join(REPO, "LICENSE"), os.path.join(resources, "LICENSE"))

    info = {k: (args.version if v == "{version}" else v) for k, v in INFO.items()}
    if args.local:
        info.update(CFBundleName=name, CFBundleDisplayName=name, CFBundleIdentifier="com.shellune.Shellune.local")
    localized = os.path.join(resources, "zh-Hans.lproj")
    os.makedirs(localized)
    with open(os.path.join(localized, "InfoPlist.strings"), "w", encoding="utf-8") as f:
        f.write('"NSLocalNetworkUsageDescription" = "连接你选择的局域网主机，以使用终端和远程桌面。";\n')
    with open(os.path.join(app, "Contents", "Info.plist"), "wb") as f:
        plistlib.dump(info, f)

    print("bundle: " + app)
    print("note:   unsigned; release builds are signed in CI")


if __name__ == "__main__":
    main()
