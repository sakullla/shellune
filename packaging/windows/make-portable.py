#!/usr/bin/env python3
"""Build the Windows portable bundle for Shellune.

Usage:
    python packaging/windows/make-portable.py --binary <path to shellune.exe> \
        [--version 0.4.1] [--out dist]

The result is a directory plus a zip:

    dist/Shellune-<version>-windows-x86_64-portable.zip
        shellune.exe
        LICENSE
        README.txt

The archive is unsigned: no code-signing certificate exists for this project, so
the readme says so instead of implying a signature.
"""

import argparse
import hashlib
import os
import shutil
import sys
import zipfile

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
NAME = "Shellune"
TAGLINE_ZH = "连接远程，也专注本地。"
TAGLINE_EN = "A modern terminal for local shells, SSH, and SFTP."

README = """{name}
{underline}

{tagline_zh}
{tagline_en}

Version {version} for Windows x86_64.

Run
---
Double-click shellune.exe. The application needs no installer, no network access
for local shells, and no administrator rights. Settings, saved hosts and accepted
server identities are stored per user under %APPDATA%\\shellune.

This build is UNSIGNED
----------------------
No code-signing certificate is available for this project, so Windows SmartScreen
may show "unknown publisher" the first time the application runs. Check the
SHA-256 published with the download before running it.

Scope
-----
Local shells, SSH sessions, host management, SSH config import and SFTP browsing,
upload, download and file management. This version does not split the window into
several terminals, does not manage jump hosts or port forwards, and file transfers
start over instead of resuming.

License
-------
GNU General Public License v3.0 only. See LICENSE.
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, help="path to the built shellune.exe")
    parser.add_argument("--version", default=None, help="version string, defaults to Cargo.toml")
    parser.add_argument("--out", default=os.path.join(REPO, "dist"), help="output directory")
    args = parser.parse_args()

    binary = os.path.abspath(args.binary)
    if not os.path.isfile(binary):
        sys.exit("binary not found: " + binary)
    if not binary.lower().endswith(".exe"):
        sys.exit("the Windows bundle needs shellune.exe, got " + binary)

    version = args.version or cargo_version()
    stem = "Shellune-{}-windows-x86_64-portable".format(version)
    bundle = os.path.join(os.path.abspath(args.out), stem)
    if os.path.isdir(bundle):
        shutil.rmtree(bundle)
    os.makedirs(bundle)

    shutil.copy2(binary, os.path.join(bundle, "shellune.exe"))
    shutil.copy2(os.path.join(REPO, "LICENSE"), os.path.join(bundle, "LICENSE"))
    with open(os.path.join(bundle, "README.txt"), "w", encoding="utf-8", newline="\r\n") as f:
        f.write(README.format(
            name=NAME,
            underline="=" * len(NAME),
            tagline_zh=TAGLINE_ZH,
            tagline_en=TAGLINE_EN,
            version=version,
        ))

    archive = bundle + ".zip"
    if os.path.exists(archive):
        os.remove(archive)
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as z:
        for entry in sorted(os.listdir(bundle)):
            z.write(os.path.join(bundle, entry), arcname=os.path.join(stem, entry))

    digest = sha256(archive)
    with open(archive + ".sha256", "w", encoding="ascii") as f:
        f.write("{}  {}\n".format(digest, os.path.basename(archive)))

    print("bundle:  " + bundle)
    print("archive: " + archive)
    print("sha256:  " + digest)


def cargo_version():
    path = os.path.join(REPO, "Cargo.toml")
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line.startswith("version") and "=" in line and "[" not in line:
                return line.split("=", 1)[1].strip().strip('"')
    sys.exit("no version in Cargo.toml")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


if __name__ == "__main__":
    main()
