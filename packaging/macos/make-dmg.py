#!/usr/bin/env python3
"""Wrap a signed Shellune.app in a drag-to-install disk image.

Usage:
    python3 packaging/macos/make-dmg.py --app dist/Shellune.app \
        [--version 0.4.0] [--out dist]

Result: dist/Shellune-<version>-macos-arm64.dmg holding Shellune.app beside an
/Applications symlink, so installing is dragging the app onto the symlink. The
image is an exact copy of the bundle: it does not sign anything, so run it after
the bundle is signed. macOS only (ditto, hdiutil).
"""

import argparse
import hashlib
import os
import shutil
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def run(command):
    print("$ " + " ".join(command))
    subprocess.run(command, check=True)


def main():
    parser = argparse.ArgumentParser(description="wrap Shellune.app in a DMG")
    parser.add_argument("--app", required=True)
    parser.add_argument("--version", default="0.4.0")
    parser.add_argument("--out", default=os.path.join(REPO, "dist"))
    args = parser.parse_args()

    app = os.path.abspath(args.app)
    if not os.path.isdir(app):
        sys.exit("app bundle not found: " + app)

    out = os.path.abspath(args.out)
    os.makedirs(out, exist_ok=True)
    staging = os.path.join(out, "dmg-staging")
    if os.path.isdir(staging):
        shutil.rmtree(staging)
    os.makedirs(staging)
    # ditto, not cp: it keeps the code signature and the bundle metadata intact.
    run(["ditto", app, os.path.join(staging, "Shellune.app")])
    os.symlink("/Applications", os.path.join(staging, "Applications"))

    dmg = os.path.join(out, "Shellune-{}-macos-arm64.dmg".format(args.version))
    run(["hdiutil", "create", "-volname", "Shellune", "-srcfolder", staging,
         "-ov", "-format", "UDZO", dmg])
    shutil.rmtree(staging)

    digest = sha256_file(dmg)
    with open(dmg + ".sha256", "w", encoding="ascii") as f:
        f.write("{}  {}\n".format(digest, os.path.basename(dmg)))

    print("image:   " + dmg)
    print("sha256:  " + digest)


if __name__ == "__main__":
    main()
